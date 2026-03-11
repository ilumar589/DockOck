//! RAG (Retrieval-Augmented Generation) pipeline for semantic cross-file context.
//!
//! Instead of using fixed 400-char excerpts from other files, this module:
//! 1. Chunks parsed document text into overlapping windows
//! 2. Embeds each chunk via Ollama's embedding API (e.g. `nomic-embed-text`)
//! 3. Stores embeddings in an in-memory vector store
//! 4. At generation time, retrieves the top-K most relevant chunks from OTHER
//!    files to inject as cross-file context

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────

/// Default embedding model available in Ollama.
pub const DEFAULT_EMBEDDING_MODEL: &str = "nomic-embed-text";

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

/// A chunk with its embedding vector.
#[derive(Debug, Clone)]
struct EmbeddedChunk {
    chunk: TextChunk,
    embedding: Vec<f32>,
}

/// A search result: a chunk with its relevance score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk: TextChunk,
    pub score: f32,
}

/// In-memory vector store holding all embedded chunks.
#[derive(Debug)]
pub struct VectorStore {
    entries: Vec<EmbeddedChunk>,
}

impl VectorStore {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Number of chunks stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert a chunk with its embedding.
    fn insert(&mut self, chunk: TextChunk, embedding: Vec<f32>) {
        self.entries.push(EmbeddedChunk { chunk, embedding });
    }

    /// Find the top-K most similar chunks to the query embedding,
    /// excluding chunks from the specified file.
    pub fn search(
        &self,
        query_embedding: &[f32],
        exclude_file: &str,
        top_k: usize,
    ) -> Vec<SearchResult> {
        let mut scored: Vec<SearchResult> = self
            .entries
            .iter()
            .filter(|e| e.chunk.file_name != exclude_file)
            .map(|e| SearchResult {
                score: cosine_similarity(query_embedding, &e.embedding),
                chunk: e.chunk.clone(),
            })
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12 {
        return 0.0;
    }
    (dot / denom) as f32
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

// ─────────────────────────────────────────────
// Embedding via Ollama API
// ─────────────────────────────────────────────

/// Response from Ollama's `/api/embed` endpoint.
#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

/// The RAG engine that owns the vector store and embedding client.
pub struct RagEngine {
    /// Ollama base URL for the embedding model (reuses an existing instance).
    endpoint_url: String,
    /// Name of the embedding model.
    model: String,
    /// The in-memory vector store.
    store: VectorStore,
    /// HTTP client (reused across requests).
    client: reqwest::Client,
    /// Disk cache for embeddings.
    cache: crate::cache::DiskCache,
}

impl RagEngine {
    /// Create a new RAG engine.
    pub fn new(endpoint_url: &str, model: &str, cache: crate::cache::DiskCache) -> Self {
        Self {
            endpoint_url: endpoint_url.to_string(),
            model: model.to_string(),
            store: VectorStore::new(),
            client: reqwest::Client::new(),
            cache,
        }
    }

    /// Number of chunks in the vector store.
    pub fn chunk_count(&self) -> usize {
        self.store.len()
    }

    /// Embed and index all chunks from a single file.
    ///
    /// Chunks are embedded in a single batch request.
    /// Results are cached by (chunk_text, model_name) hash.
    pub async fn index_file(
        &mut self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
    ) -> Result<usize> {
        let chunks = chunk_text(raw_text, file_name, file_type);
        if chunks.is_empty() {
            return Ok(0);
        }

        // Separate cached vs uncached chunks
        let mut cached_embeddings: HashMap<usize, Vec<f32>> = HashMap::new();
        let mut uncached_indices: Vec<usize> = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let cache_key = crate::cache::composite_key(&[
                chunk.text.as_bytes(),
                self.model.as_bytes(),
            ]);
            if let Some(emb) = self.cache.get::<Vec<f32>>(crate::cache::NS_EMBEDDING, &cache_key) {
                cached_embeddings.insert(i, emb);
            } else {
                uncached_indices.push(i);
            }
        }

        // Embed uncached chunks in batch
        if !uncached_indices.is_empty() {
            let texts: Vec<&str> = uncached_indices
                .iter()
                .map(|&i| chunks[i].text.as_str())
                .collect();

            let embeddings = self.embed_batch(&texts).await?;

            if embeddings.len() != uncached_indices.len() {
                anyhow::bail!(
                    "Embedding count mismatch: expected {}, got {}",
                    uncached_indices.len(),
                    embeddings.len()
                );
            }

            for (batch_idx, &chunk_idx) in uncached_indices.iter().enumerate() {
                let emb = &embeddings[batch_idx];
                // Cache the embedding
                let cache_key = crate::cache::composite_key(&[
                    chunks[chunk_idx].text.as_bytes(),
                    self.model.as_bytes(),
                ]);
                self.cache.put(crate::cache::NS_EMBEDDING, &cache_key, emb);
                cached_embeddings.insert(chunk_idx, emb.clone());
            }
        }

        // Insert all chunks into the vector store
        let count = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            if let Some(emb) = cached_embeddings.remove(&i) {
                self.store.insert(chunk, emb);
            }
        }

        Ok(count)
    }

    /// Retrieve the top-K most relevant chunks from files other than `exclude_file`.
    ///
    /// The query text is embedded, then used to search the vector store.
    pub async fn retrieve(
        &self,
        query_text: &str,
        exclude_file: &str,
    ) -> Result<Vec<SearchResult>> {
        let query_emb = self.embed_single(query_text).await?;
        Ok(self.store.search(&query_emb, exclude_file, TOP_K))
    }

    /// Build a formatted cross-file context string from RAG retrieval results.
    ///
    /// The query text should be a representative summary or the preprocessed text
    /// of the current file being processed.
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

    /// Embed a single text string.
    async fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let batch = self.embed_batch(&[text]).await?;
        batch
            .into_iter()
            .next()
            .context("Empty embedding response for single text")
    }

    /// Embed a batch of texts via Ollama's `/api/embed` endpoint.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post(format!("{}/api/embed", self.endpoint_url))
            .json(&serde_json::json!({
                "model": self.model,
                "input": texts,
            }))
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .context("Embedding API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Embedding API returned {}: {}", status, body);
        }

        let body: OllamaEmbedResponse = resp
            .json()
            .await
            .context("Failed to parse embedding response")?;

        Ok(body.embeddings)
    }
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
