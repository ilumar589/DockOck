//! RAG (Retrieval-Augmented Generation) pipeline for semantic cross-file context.
//!
//! Instead of using fixed 400-char excerpts from other files, this module:
//! 1. Chunks parsed document text into overlapping windows
//! 2. Embeds each chunk via a local TF-IDF hashing vectorizer (no API needed)
//! 3. Stores embeddings in a simple in-memory Vec (brute-force cosine search)
//! 4. At generation time, retrieves the top-K most relevant chunks from OTHER
//!    files to inject as cross-file context
//!
//! Performance optimisations:
//! - **Skip indexed**: files already stored (by name + chunk count) are skipped
//! - **Disk-cached embeddings**: individual vectors cached by content hash
//! - **Sub-batching**: large files are embedded in batches of 8
//! - **Brute-force search**: at typical scale (~3 000 chunks × 768-dim) this is
//!   sub-millisecond and avoids the overhead/hangs of an embedded vector DB.

use anyhow::Result;
use serde::{Deserialize, Serialize};
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

/// A stored chunk with its precomputed embedding vector.
struct StoredChunk {
    chunk: TextChunk,
    embedding: Vec<f32>,
}

/// The RAG engine backed by a simple in-memory vector store with brute-force
/// cosine similarity search.  At the scale we operate (~3 000 chunks, 768-dim)
/// this is sub-millisecond and avoids all SurrealDB overhead/hangs.
///
/// No disk cache is used — TF-IDF embedding is pure CPU and faster to
/// recompute than to read/write a (potentially network-mounted) cache dir.
pub struct RagEngine {
    /// Embedding vector dimension.
    embed_dim: usize,
    /// All stored chunks + embeddings.
    store: Vec<StoredChunk>,
    /// Set of file names already indexed (for skip-if-present).
    indexed_files: std::collections::HashMap<String, usize>,
}

impl RagEngine {
    /// Create a new RAG engine with in-memory vector storage.
    ///
    /// * `embed_dim` — embedding vector dimension
    pub fn new(embed_dim: usize) -> Self {
        info!("RAG engine: using in-memory vector store (brute-force cosine, no disk cache)");
        Self {
            embed_dim,
            store: Vec::new(),
            indexed_files: std::collections::HashMap::new(),
        }
    }

    /// Number of chunks in the vector store.
    pub fn chunk_count(&self) -> usize {
        self.store.len()
    }

    /// Embed and index all chunks from a single file (synchronous).
    ///
    /// Pure CPU work — call from a blocking context, NOT from the async runtime.
    pub fn index_file(
        &mut self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
    ) -> IndexResult {
        let chunks = chunk_text(raw_text, file_name, file_type);
        if chunks.is_empty() {
            return IndexResult { total_chunks: 0, cached: 0, embedded: 0, skipped: true };
        }

        // Skip if this file is already fully indexed in memory
        if let Some(&existing_count) = self.indexed_files.get(file_name) {
            if existing_count == chunks.len() {
                return IndexResult {
                    total_chunks: chunks.len(),
                    cached: 0,
                    embedded: 0,
                    skipped: true,
                };
            }
            // Chunk count changed — remove stale chunks and re-index
            self.store.retain(|sc| sc.chunk.file_name != file_name);
        }

        let dim = self.embed_dim;
        let embeddings: Vec<Vec<f32>> = chunks
            .iter()
            .map(|c| tfidf_hash_embed(&c.text, dim))
            .collect();

        let count = chunks.len();
        for (chunk, emb) in chunks.into_iter().zip(embeddings) {
            self.store.push(StoredChunk { chunk, embedding: emb });
        }

        self.indexed_files.insert(file_name.to_string(), count);
        IndexResult { total_chunks: count, cached: 0, embedded: count, skipped: false }
    }

    /// Retrieve the top-K most relevant chunks from files other than `exclude_file`.
    ///
    /// Brute-force cosine similarity — fast enough for thousands of 768-dim vectors.
    pub async fn retrieve(
        &self,
        query_text: &str,
        exclude_file: &str,
    ) -> Result<Vec<SearchResult>> {
        let query_emb = self.embed_single(query_text);

        // Score every chunk (excluding the query's own file)
        let mut scored: Vec<(usize, f32)> = self.store.iter().enumerate()
            .filter(|(_, sc)| sc.chunk.file_name != exclude_file)
            .map(|(i, sc)| (i, cosine_similarity(&query_emb, &sc.embedding)))
            .collect();

        // Partial sort: top-K by descending score
        scored.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(TOP_K);

        Ok(scored.into_iter().map(|(i, score)| {
            SearchResult {
                chunk: self.store[i].chunk.clone(),
                score,
            }
        }).collect())
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

            let entry_cost = header.len() + snippet.len() + 2;
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

    /// Clear all chunks for a fresh run.
    #[allow(dead_code)]
    pub async fn clear(&mut self) -> Result<()> {
        self.store.clear();
        self.indexed_files.clear();
        Ok(())
    }

    /// Embed a single text string (pure CPU, no disk cache).
    fn embed_single(&self, text: &str) -> Vec<f32> {
        tfidf_hash_embed(text, self.embed_dim)
    }

}

/// Cosine similarity between two vectors (assumes both are L2-normalised).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
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
