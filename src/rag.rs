//! RAG (Retrieval-Augmented Generation) pipeline for semantic cross-file context.
//!
//! Instead of using fixed 400-char excerpts from other files, this module:
//! 1. Chunks parsed document text into overlapping windows
//! 2. Embeds each chunk via a local TF-IDF hashing vectorizer (no API needed)
//! 3. Stores embeddings in SurrealDB (embedded mode with HNSW vector indexing)
//! 4. At generation time, retrieves the top-K most relevant chunks from OTHER
//!    files to inject as cross-file context
//!
//! Performance optimisations:
//! - **Skip indexed**: files already in SurrealDB (by name + chunk count) are skipped
//! - **Disk-cached embeddings**: individual vectors cached by content hash
//! - **Sub-batching**: large files are embedded in batches of 8
//!
//! Storage: SurrealKV (persistent, pure-Rust) when an output dir is available;
//! in-memory fallback otherwise. Persisted embeddings survive across app restarts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::local::Db;
use tracing::info;

// ─────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────

/// Dimension for local TF-IDF hashing vectors.
pub const LOCAL_EMBEDDING_DIM: usize = 768;

/// Number of characters per chunk (~128 tokens × 4 chars/token = 512 chars).
const CHUNK_SIZE_CHARS: usize = 2048;

/// Overlap between consecutive chunks (in characters).
const CHUNK_OVERLAP_CHARS: usize = 512;

/// How many top chunks to retrieve per query.
const TOP_K: usize = 8;

/// Maximum total characters of cross-file context to inject into the prompt.
const MAX_CONTEXT_CHARS: usize = 8_000;

// ─────────────────────────────────────────────
// Data types
// ─────────────────────────────────────────────

/// A chunk of text from a parsed document, with its source metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChunk {
    /// Source file name (e.g. "D028.docx")
    pub file_name: String,
    /// Source file type label (e.g. "Word", "Excel", "Visio")
    pub file_type: String,
    /// The chunk text itself
    pub text: String,
    /// Character offset in the original document
    pub offset: usize,
    /// Chunk index within the file (0-based)
    pub chunk_index: usize,
}

/// A search result: a chunk with its relevance score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk: TextChunk,
    pub score: f32,
}

/// Row shape returned from the vector similarity SurrealQL query.
#[derive(Debug, Deserialize)]
struct ChunkRow {
    file_name: String,
    file_type: String,
    text: String,
    offset: usize,
    chunk_index: usize,
    score: f32,
}

/// Row shape for INSERT (includes the embedding vector).
#[derive(Debug, Serialize)]
struct ChunkInsert {
    file_name: String,
    file_type: String,
    text: String,
    offset: usize,
    chunk_index: usize,
    embedding: Vec<f32>,
}

// ─────────────────────────────────────────────
// Chunking
// ─────────────────────────────────────────────

/// Split document text into overlapping chunks.
pub fn chunk_text(
    text: &str,
    file_name: &str,
    file_type: &str,
) -> Vec<TextChunk> {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();

    if total == 0 {
        return Vec::new();
    }

    // If the text fits in a single chunk, return it as-is.
    if total <= CHUNK_SIZE_CHARS {
        return vec![TextChunk {
            file_name: file_name.to_string(),
            file_type: file_type.to_string(),
            text: text.to_string(),
            offset: 0,
            chunk_index: 0,
        }];
    }

    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let mut chunk_idx = 0usize;
    let step = CHUNK_SIZE_CHARS - CHUNK_OVERLAP_CHARS;

    while offset < total {
        let end = (offset + CHUNK_SIZE_CHARS).min(total);
        let chunk_text: String = chars[offset..end].iter().collect();

        // Try to break at a line boundary if possible (within last 20% of chunk)
        let actual_text = if end < total {
            snap_to_line_boundary(&chunk_text)
        } else {
            chunk_text
        };

        if !actual_text.trim().is_empty() {
            chunks.push(TextChunk {
                file_name: file_name.to_string(),
                file_type: file_type.to_string(),
                text: actual_text,
                offset,
                chunk_index: chunk_idx,
            });
            chunk_idx += 1;
        }

        offset += step;
    }

    chunks
}

/// Try to snap the chunk boundary to the last newline in the final 20% of the text,
/// so we don't split mid-sentence.
fn snap_to_line_boundary(text: &str) -> String {
    let len = text.len();
    let search_start = len.saturating_sub(len / 5); // last 20%
    if let Some(pos) = text[search_start..].rfind('\n') {
        text[..search_start + pos + 1].to_string()
    } else {
        text.to_string()
    }
}

/// Maximum texts per embedding sub-batch.
const EMBED_SUB_BATCH: usize = 8;

/// The RAG engine backed by SurrealDB embedded with HNSW vector indexing.
pub struct RagEngine {
    /// Embedding vector dimension.
    embed_dim: usize,
    /// SurrealDB embedded instance.
    db: Surreal<Db>,
    /// Running count of chunks stored in this session.
    chunk_count: usize,
    /// Disk cache for embedding vectors (avoids re-computing for identical text).
    cache: crate::cache::DiskCache,
    /// Set of file names already indexed in SurrealDB (for skip-if-present).
    indexed_files: std::collections::HashMap<String, usize>,
}

/// SurrealQL schema migration run on startup.
/// The HNSW dimension is set based on the embedding backend:
/// - Ollama `nomic-embed-text`: 768 dimensions
/// - Cloud embedding models: typically 1024 or 2048 dimensions
fn schema_sql(dim: usize) -> String {
    format!(r#"
DEFINE TABLE IF NOT EXISTS chunks SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS file_name   ON chunks TYPE string;
DEFINE FIELD IF NOT EXISTS file_type   ON chunks TYPE string;
DEFINE FIELD IF NOT EXISTS text        ON chunks TYPE string;
DEFINE FIELD IF NOT EXISTS offset      ON chunks TYPE int;
DEFINE FIELD IF NOT EXISTS chunk_index ON chunks TYPE int;
DEFINE FIELD IF NOT EXISTS embedding   ON chunks TYPE array<float>;
DEFINE INDEX IF NOT EXISTS idx_chunks_embedding ON chunks
    FIELDS embedding HNSW DIMENSION {dim} DIST COSINE;
DEFINE INDEX IF NOT EXISTS idx_chunks_file ON chunks FIELDS file_name;
"#)
}

impl RagEngine {
    /// Create a new RAG engine with SurrealDB storage.
    ///
    /// * `surreal_path` — if `Some`, SurrealKV persistent storage at that dir;
    ///   if `None`, in-memory only.
    /// * `embed_dim` — embedding vector dimension
    /// * `cache` — disk cache for embedding vectors
    pub async fn new(
        surreal_path: Option<&std::path::Path>,
        embed_dim: usize,
        cache: crate::cache::DiskCache,
    ) -> Result<Self> {
        let db = if let Some(path) = surreal_path {
            let dir = path.join(".dockock_surreal");
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create SurrealDB dir: {}", dir.display()))?;
            let path_str = dir.to_string_lossy().to_string();
            info!("SurrealDB: opening persistent store at {}", path_str);
            Surreal::new::<surrealdb::engine::local::SurrealKv>(path_str.as_str())
                .await
                .context("Failed to open SurrealKV database")?
        } else {
            info!("SurrealDB: using in-memory store (no output dir)");
            Surreal::new::<surrealdb::engine::local::Mem>(())
                .await
                .context("Failed to open in-memory SurrealDB")?
        };

        db.use_ns("dockock").use_db("rag").await
            .context("Failed to select SurrealDB namespace/database")?;

        // Run schema migration with the correct embedding dimension
        db.query(&schema_sql(embed_dim)).await
            .context("Failed to run SurrealDB schema migration")?;

        // Count existing chunks (from a previous run if persistent)
        let mut result = db.query("SELECT count() AS c FROM chunks GROUP ALL")
            .await
            .context("Failed to count existing chunks")?;
        let existing: Option<CountRow> = result.take(0).ok().and_then(|v: Vec<CountRow>| v.into_iter().next());
        let chunk_count = existing.map(|r| r.c).unwrap_or(0);
        if chunk_count > 0 {
            info!("SurrealDB: loaded {} existing chunks from previous session", chunk_count);
        }

        // Build index of which files are already in SurrealDB (for skip-if-present)
        let mut idx_result = db
            .query("SELECT file_name, count() AS c FROM chunks GROUP BY file_name")
            .await
            .context("Failed to query indexed files")?;
        let idx_rows: Vec<FileCountRow> = idx_result.take(0).unwrap_or_default();
        let indexed_files: std::collections::HashMap<String, usize> = idx_rows
            .into_iter()
            .map(|r| (r.file_name, r.c))
            .collect();
        if !indexed_files.is_empty() {
            info!("SurrealDB: {} files already indexed", indexed_files.len());
        }

        Ok(Self {
            embed_dim,
            db,
            chunk_count,
            cache,
            indexed_files,
        })
    }

    /// Number of chunks in the vector store.
    pub fn chunk_count(&self) -> usize {
        self.chunk_count
    }

    /// Embed and index all chunks from a single file.
    ///
    /// Chunks are embedded in sub-batches with disk-cache lookups.
    /// Files already present in SurrealDB (matching chunk count) are skipped entirely.
    pub async fn index_file(
        &mut self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
    ) -> Result<IndexResult> {
        let chunks = chunk_text(raw_text, file_name, file_type);
        if chunks.is_empty() {
            return Ok(IndexResult { total_chunks: 0, cached: 0, embedded: 0, skipped: true });
        }

        // Skip if this file is already fully indexed in SurrealDB
        if let Some(&existing_count) = self.indexed_files.get(file_name) {
            if existing_count == chunks.len() {
                return Ok(IndexResult {
                    total_chunks: chunks.len(),
                    cached: 0,
                    embedded: 0,
                    skipped: true,
                });
            }
            // Chunk count changed (doc was edited) — delete stale chunks and re-index
            self.db
                .query("DELETE chunks WHERE file_name = $fname")
                .bind(("fname", file_name.to_string()))
                .await
                .context("Failed to delete stale chunks")?;
        }

        // Embed with sub-batching + disk cache
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
        let mut cached_count = 0usize;
        let mut embedded_count = 0usize;

        for batch_start in (0..chunks.len()).step_by(EMBED_SUB_BATCH) {
            let batch_end = (batch_start + EMBED_SUB_BATCH).min(chunks.len());
            let batch = &chunks[batch_start..batch_end];

            // Check disk cache for each text in the batch
            let mut batch_results: Vec<Option<Vec<f32>>> = Vec::with_capacity(batch.len());
            let mut uncached_indices: Vec<usize> = Vec::new();
            let mut uncached_texts: Vec<&str> = Vec::new();

            for (i, chunk) in batch.iter().enumerate() {
                let cache_key = crate::cache::composite_key(&[
                    chunk.text.as_bytes(),
                    b"local-tfidf",
                ]);
                if let Some(cached_emb) = self.cache.get::<Vec<f32>>(crate::cache::NS_EMBEDDING, &cache_key) {
                    batch_results.push(Some(cached_emb));
                    cached_count += 1;
                } else {
                    batch_results.push(None);
                    uncached_indices.push(i);
                    uncached_texts.push(&chunk.text);
                }
            }

            // Embed uncached texts locally
            if !uncached_texts.is_empty() {
                let new_embeddings = self.embed_batch(&uncached_texts);
                if new_embeddings.len() != uncached_texts.len() {
                    anyhow::bail!(
                        "Embedding count mismatch: expected {}, got {}",
                        uncached_texts.len(),
                        new_embeddings.len()
                    );
                }
                // Merge results and cache new embeddings
                for (idx_in_uncached, &local_idx) in uncached_indices.iter().enumerate() {
                    let emb = new_embeddings[idx_in_uncached].clone();
                    // Cache on disk
                    let cache_key = crate::cache::composite_key(&[
                        batch[local_idx].text.as_bytes(),
                        b"local-tfidf",
                    ]);
                    self.cache.put(crate::cache::NS_EMBEDDING, &cache_key, &emb);
                    batch_results[local_idx] = Some(emb);
                }
                embedded_count += uncached_texts.len();
            }

            // Collect final embeddings for this sub-batch
            for result in batch_results {
                embeddings.push(result.expect("all embeddings should be resolved"));
            }
        }

        // Batch-insert all chunks into SurrealDB via a single query
        let count = chunks.len();
        for (chunk, emb) in chunks.into_iter().zip(embeddings.into_iter()) {
            let row = ChunkInsert {
                file_name: chunk.file_name,
                file_type: chunk.file_type,
                text: chunk.text,
                offset: chunk.offset,
                chunk_index: chunk.chunk_index,
                embedding: emb,
            };
            self.db
                .create::<Option<serde_json::Value>>("chunks")
                .content(row)
                .await
                .context("Failed to insert chunk into SurrealDB")?;
        }

        self.chunk_count += count;
        self.indexed_files.insert(file_name.to_string(), count);
        Ok(IndexResult { total_chunks: count, cached: cached_count, embedded: embedded_count, skipped: false })
    }

    /// Retrieve the top-K most relevant chunks from files other than `exclude_file`.
    ///
    /// Uses SurrealDB's built-in cosine similarity via HNSW index.
    pub async fn retrieve(
        &self,
        query_text: &str,
        exclude_file: &str,
    ) -> Result<Vec<SearchResult>> {
        let query_emb = self.embed_single(query_text).await?;

        let sql = "
            SELECT
                file_name, file_type, text, offset, chunk_index,
                vector::similarity::cosine(embedding, $query_emb) AS score
            FROM chunks
            WHERE file_name != $exclude
            ORDER BY score DESC
            LIMIT $top_k
        ";

        let mut result = self.db
            .query(sql)
            .bind(("query_emb", query_emb))
            .bind(("exclude", exclude_file.to_string()))
            .bind(("top_k", TOP_K as i64))
            .await
            .context("SurrealDB vector search failed")?;

        let rows: Vec<ChunkRow> = result.take(0)
            .unwrap_or_default();

        Ok(rows
            .into_iter()
            .map(|r| SearchResult {
                chunk: TextChunk {
                    file_name: r.file_name,
                    file_type: r.file_type,
                    text: r.text,
                    offset: r.offset,
                    chunk_index: r.chunk_index,
                },
                score: r.score,
            })
            .collect())
    }

    /// Build a formatted cross-file context string from RAG retrieval results.
    pub async fn build_cross_file_context(
        &self,
        query_text: &str,
        exclude_file: &str,
    ) -> Result<String> {
        let results = self.retrieve(query_text, exclude_file).await?;

        if results.is_empty() {
            return Ok("No relevant cross-file context found.".to_string());
        }

        let mut context = String::from("=== Related Context from Other Documents ===\n\n");
        let mut chars_used = context.len();

        for result in results.iter() {
            let header = format!(
                "[From {} ({}), chunk {}] (relevance: {:.2})\n",
                result.chunk.file_name,
                result.chunk.file_type,
                result.chunk.chunk_index + 1,
                result.score,
            );
            let snippet = &result.chunk.text;

            let entry_cost = header.len() + snippet.len() + 2; // +2 for newlines
            if chars_used + entry_cost > MAX_CONTEXT_CHARS {
                let remaining = MAX_CONTEXT_CHARS.saturating_sub(chars_used + header.len() + 10);
                if remaining > 100 {
                    context.push_str(&header);
                    let truncated: String = snippet.chars().take(remaining).collect();
                    context.push_str(&truncated);
                    context.push_str("…\n\n");
                }
                break;
            }

            context.push_str(&header);
            context.push_str(snippet);
            context.push_str("\n\n");
            chars_used += entry_cost;
        }

        Ok(context)
    }

    /// Clear all chunks for a fresh run (useful when force-regenerating).
    #[allow(dead_code)]
    pub async fn clear(&mut self) -> Result<()> {
        self.db.query("DELETE chunks").await
            .context("Failed to clear chunks table")?;
        self.chunk_count = 0;
        Ok(())
    }

    /// Embed a single text string.
    async fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let cache_key = crate::cache::composite_key(&[
            text.as_bytes(),
            b"local-tfidf",
        ]);
        if let Some(cached) = self.cache.get::<Vec<f32>>(crate::cache::NS_EMBEDDING, &cache_key) {
            return Ok(cached);
        }

        let emb = tfidf_hash_embed(text, self.embed_dim);
        self.cache.put(crate::cache::NS_EMBEDDING, &cache_key, &emb);
        Ok(emb)
    }

    /// Embed a batch of texts using local TF-IDF hashing.
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| tfidf_hash_embed(t, self.embed_dim)).collect()
    }
}

/// Result of indexing a single file — provides detail for progress reporting.
#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Total chunk count for the file.
    pub total_chunks: usize,
    /// How many chunks had cached embeddings (disk cache hit).
    pub cached: usize,
    /// How many chunks were freshly embedded via API.
    pub embedded: usize,
    /// Whether the file was skipped (already fully indexed in SurrealDB).
    pub skipped: bool,
}

/// Helper for COUNT query deserialization.
#[derive(Debug, Deserialize)]
struct CountRow {
    c: usize,
}

/// Helper for per-file COUNT query deserialization.
#[derive(Debug, Deserialize)]
struct FileCountRow {
    file_name: String,
    c: usize,
}

// ─────────────────────────────────────────────
// Local TF-IDF hashing vectorizer
// ─────────────────────────────────────────────

/// Tokenize text into lowercase word tokens (≥ 2 chars, alphanumeric only).
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_lowercase())
        .collect()
}

/// FNV-1a–style hash → bucket index.
fn hash_token(token: &str, dim: usize) -> usize {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in token.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h as usize) % dim
}

/// Sign hash for the signed feature-hashing trick (reduces collision bias).
fn sign_hash(token: &str) -> f32 {
    let mut h: u64 = 0x517cc1b727220a95;
    for b in token.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    if h & 1 == 0 { 1.0 } else { -1.0 }
}

/// Produce a TF-IDF–like embedding using the feature-hashing trick.
///
/// Uses sublinear TF (`1 + ln(count)`) and signed hashing.  The resulting
/// vector is L2-normalised so cosine similarity works directly.
fn tfidf_hash_embed(text: &str, dim: usize) -> Vec<f32> {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return vec![0.0; dim];
    }

    // Count term frequencies
    let mut tf: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for t in &tokens {
        *tf.entry(t.as_str()).or_insert(0) += 1;
    }

    // Feature hashing with sublinear TF
    let mut vec = vec![0.0f32; dim];
    for (term, &count) in &tf {
        let idx = hash_token(term, dim);
        let sign = sign_hash(term);
        let weight = 1.0 + (count as f32).ln();
        vec[idx] += sign * weight;
    }

    // L2 normalise
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vec {
            *x /= norm;
        }
    }

    vec
}

/// Build a short representative query from a file's raw text for RAG retrieval.
///
/// Takes the first ~1000 chars plus any headings/key terms found in the document.
pub fn build_query_text(raw_text: &str, file_name: &str) -> String {
    let mut query = format!("Document: {}\n", file_name);

    // Collect headings and key lines (first pass)
    let mut key_lines = Vec::new();
    let mut other_text = String::new();
    for line in raw_text.lines().take(200) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if trimmed.starts_with('#')
            || lower.starts_with("section")
            || lower.starts_with("chapter")
            || lower.contains("requirement")
            || lower.contains("process")
            || lower.contains("workflow")
            || lower.contains("feature")
        {
            key_lines.push(trimmed.to_string());
        } else if other_text.len() < 800 {
            other_text.push_str(trimmed);
            other_text.push('\n');
        }
    }

    for line in &key_lines {
        query.push_str(line);
        query.push('\n');
    }
    query.push_str(&other_text);

    // Cap query at ~1500 chars
    if query.len() > 1500 {
        query.truncate(1500);
    }

    query
}
