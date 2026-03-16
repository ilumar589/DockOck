//! RAG pipeline using rig-core embedding APIs + MongoDB vector store (via Docker).
//!
//! Replaces the old TF-IDF hashing approach with semantic embeddings:
//! 1. Chunks parsed document text into overlapping windows
//! 2. Embeds each chunk via Ollama / OpenAI / FastEmbed (local CPU fallback)
//! 3. Stores embeddings in MongoDB (persistent across runs)
//! 4. At generation time, retrieves the top-N most relevant chunks from OTHER
//!    files to inject as cross-file context

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use mongodb::bson::{self, doc};
use mongodb::options::ClientOptions;
use mongodb::{Client as MongoClient, Collection};
use rig::client::EmbeddingsClient;
use rig::embeddings::EmbeddingsBuilder;
use rig::vector_store::{
    TopNResults, VectorSearchRequest, VectorStoreError, VectorStoreIndex, VectorStoreIndexDyn,
    request::Filter,
};
use rig_fastembed::FastembedModel;
use rig_mongodb::{MongoDbVectorIndex, SearchParams};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

// ─────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────

/// Number of characters per chunk (~256 tokens).
const CHUNK_SIZE_CHARS: usize = 1024;

/// Overlap between consecutive chunks (in characters), ~25%.
const CHUNK_OVERLAP_CHARS: usize = 256;

/// How many top chunks to retrieve per query.
const TOP_K: usize = 4;

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

impl TextChunk {
    /// Unique document ID for MongoDB storage: `"filename:chunk_idx"`.
    pub fn document_id(&self) -> String {
        format!("{}:{}", self.file_name, self.chunk_index)
    }
}

// ─────────────────────────────────────────────
// Chunking
// ─────────────────────────────────────────────

/// Split document text into overlapping chunks, snapping to line boundaries.
pub fn chunk_text(text: &str, file_name: &str, file_type: &str) -> Vec<TextChunk> {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();

    if total == 0 {
        return Vec::new();
    }

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

/// Snap the chunk boundary to the last newline in the final 20% of the text.
fn snap_to_line_boundary(text: &str) -> String {
    let len = text.len();
    let search_start = len.saturating_sub(len / 5);
    if let Some(pos) = text[search_start..].rfind('\n') {
        text[..search_start + pos + 1].to_string()
    } else {
        text.to_string()
    }
}

// ─────────────────────────────────────────────
// MongoDB connection
// ─────────────────────────────────────────────

/// Connect to the MongoDB instance running in Docker.
/// Returns the `chunks` collection in the `dockock` database.
pub async fn connect_mongo(connection_string: &str) -> Result<MongoClient> {
    let options = ClientOptions::parse(connection_string).await?;
    let client = MongoClient::with_options(options)?;
    // Ping to verify connectivity
    client
        .database("dockock")
        .run_command(doc! { "ping": 1 })
        .await?;
    info!("Connected to MongoDB at {}", connection_string);
    Ok(client)
}

/// Get the chunks collection.
pub fn chunks_collection(client: &MongoClient) -> Collection<bson::Document> {
    client.database("dockock").collection("chunks")
}

/// Ensure that the required Atlas Search vector indexes exist on both the
/// `chunks` and `memories` collections.  On `mongodb-atlas-local` the indexes
/// are created via `createSearchIndexes`; if they already exist the command
/// returns an error that we silently ignore.
#[instrument(skip_all)]
pub async fn ensure_search_indexes(client: &MongoClient) {
    let db = client.database("dockock");

    // Index for `chunks` collection
    let chunks_cmd = doc! {
        "createSearchIndexes": "chunks",
        "indexes": [{
            "name": "vector_index",
            "type": "vectorSearch",
            "definition": {
                "fields": [{
                    "type": "vector",
                    "path": "embedding",
                    "numDimensions": 768,
                    "similarity": "cosine"
                }]
            }
        }]
    };
    match db.run_command(chunks_cmd).await {
        Ok(_) => info!("Created vector_index on chunks collection"),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") || msg.contains("Duplicate") {
                info!("vector_index already exists on chunks collection");
            } else {
                warn!("Failed to create vector_index on chunks: {e}");
            }
        }
    }

    // Index for `memories` collection
    let memories_cmd = doc! {
        "createSearchIndexes": "memories",
        "indexes": [{
            "name": "memory_vector_index",
            "type": "vectorSearch",
            "definition": {
                "fields": [{
                    "type": "vector",
                    "path": "embedding",
                    "numDimensions": 768,
                    "similarity": "cosine"
                }]
            }
        }]
    };
    match db.run_command(memories_cmd).await {
        Ok(_) => info!("Created memory_vector_index on memories collection"),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") || msg.contains("Duplicate") {
                info!("memory_vector_index already exists on memories collection");
            } else {
                warn!("Failed to create memory_vector_index on memories: {e}");
            }
        }
    }
}

// ─────────────────────────────────────────────
// Embedding provider selection
// ─────────────────────────────────────────────

/// User-facing embedding model selection in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingChoice {
    /// Try Ollama first, fall back to FastEmbed.
    Auto,
    /// Ollama `nomic-embed-text` (768-dim, GPU-accelerated).
    OllamaNomicEmbedText,
    /// Ollama `mxbai-embed-large` (1024-dim, higher quality).
    OllamaMxbaiEmbedLarge,
    /// FastEmbed local CPU (`AllMiniLML6V2Q`, 384-dim, no external service).
    FastEmbedMiniLM,
    /// Disable RAG entirely; use excerpt-based context fallback.
    None,
}

impl EmbeddingChoice {
    pub const ALL: &[EmbeddingChoice] = &[
        Self::Auto,
        Self::OllamaNomicEmbedText,
        Self::OllamaMxbaiEmbedLarge,
        Self::FastEmbedMiniLM,
        Self::None,
    ];
}

impl std::fmt::Display for EmbeddingChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "Auto (Ollama → FastEmbed)"),
            Self::OllamaNomicEmbedText => write!(f, "nomic-embed-text (Ollama)"),
            Self::OllamaMxbaiEmbedLarge => write!(f, "mxbai-embed-large (Ollama)"),
            Self::FastEmbedMiniLM => write!(f, "AllMiniLM (FastEmbed, CPU)"),
            Self::None => write!(f, "None (disable RAG)"),
        }
    }
}

impl Default for EmbeddingChoice {
    fn default() -> Self {
        Self::Auto
    }
}

/// Which embedding backend to use.
#[derive(Debug, Clone)]
pub enum EmbeddingProvider {
    /// Use the Ollama instance for embeddings (e.g. nomic-embed-text).
    Ollama {
        client: rig::providers::ollama::Client,
        model: String,
    },
    /// Use FastEmbed for local CPU-only embeddings (no external service needed).
    FastEmbed,
}

// ─────────────────────────────────────────────
// Index building (Phase 1.3)
// ─────────────────────────────────────────────

/// Build the RAG index by embedding all project file chunks and upserting
/// them into MongoDB. Returns `true` if index was built successfully.
///
/// Falls back gracefully: if embedding fails, returns `false` so
/// the caller can use excerpt-based context instead.
#[instrument(skip_all, fields(chunk_count))]
pub async fn build_index(
    provider: &EmbeddingProvider,
    chunks: &[TextChunk],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<bool> {
    if chunks.is_empty() {
        info!("RAG: no chunks to index");
        return Ok(false);
    }
    tracing::Span::current().record("chunk_count", chunks.len());

    match provider {
        EmbeddingProvider::Ollama { client, model } => {
            build_index_ollama(client, model, chunks, collection, cancel_token).await
        }
        EmbeddingProvider::FastEmbed => {
            build_index_fastembed(chunks, collection, cancel_token).await
        }
    }
}

/// Build index using Ollama embeddings.
async fn build_index_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    chunks: &[TextChunk],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<bool> {
    let model = client.embedding_model(model_name);
    let ids: Vec<String> = chunks.iter().map(|c| c.document_id()).collect();
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();

    let embeddings = tokio::select! {
        result = EmbeddingsBuilder::new(model)
            .documents(texts)?
            .build() => {
            match result {
                Ok(e) => e,
                Err(e) => {
                    warn!("RAG: Ollama embedding failed: {e}");
                    return Ok(false);
                }
            }
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    };

    upsert_embeddings(&ids, &embeddings, collection).await?;
    info!("RAG index built via Ollama ({} chunks)", embeddings.len());
    Ok(true)
}

/// Build index using FastEmbed (local CPU fallback).
async fn build_index_fastembed(
    chunks: &[TextChunk],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<bool> {
    let fastembed_client = rig_fastembed::Client::new();
    let model = fastembed_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let ids: Vec<String> = chunks.iter().map(|c| c.document_id()).collect();
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();

    let embeddings = tokio::select! {
        result = EmbeddingsBuilder::new(model)
            .documents(texts)?
            .build() => {
            match result {
                Ok(e) => e,
                Err(e) => {
                    warn!("RAG: FastEmbed embedding failed: {e}");
                    return Ok(false);
                }
            }
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    };

    upsert_embeddings(&ids, &embeddings, collection).await?;
    info!("RAG index built via FastEmbed ({} chunks)", embeddings.len());
    Ok(true)
}

/// Upsert embedded documents into MongoDB.
async fn upsert_embeddings(
    ids: &[String],
    embeddings: &[(String, rig::one_or_many::OneOrMany<rig::embeddings::Embedding>)],
    collection: &Collection<bson::Document>,
) -> Result<()> {
    for (id, (text, emb_set)) in ids.iter().zip(embeddings.iter()) {
        let embedding = emb_set.first_ref();
        let d = doc! {
            "_id": id.clone(),
            "text": text.clone(),
            "embedding": &embedding.vec,
        };
        collection
            .replace_one(doc! { "_id": id.clone() }, d)
            .upsert(true)
            .await?;
    }
    Ok(())
}

// ─────────────────────────────────────────────
// Retrieval (Phase 2 per-file)
// ─────────────────────────────────────────────

/// Retrieve cross-file context for a given file by querying MongoDB vector index.
///
/// Returns a formatted context string, or empty string if retrieval fails.
#[instrument(skip_all, fields(exclude_file, result_count))]
#[allow(dead_code)]
pub async fn retrieve_context(
    provider: &EmbeddingProvider,
    collection: &Collection<bson::Document>,
    query_text: &str,
    exclude_file: &str,
    cancel_token: &CancellationToken,
) -> String {
    let result = match provider {
        EmbeddingProvider::Ollama { client, model } => {
            retrieve_ollama(client, model, collection, query_text, exclude_file, cancel_token).await
        }
        EmbeddingProvider::FastEmbed => {
            retrieve_fastembed(collection, query_text, exclude_file, cancel_token).await
        }
    };

    match result {
        Ok(ctx) => {
            tracing::Span::current().record("result_count", ctx.len());
            ctx
        }
        Err(e) => {
            warn!("RAG retrieval failed: {e}");
            String::new()
        }
    }
}

#[allow(dead_code)]
async fn retrieve_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    collection: &Collection<bson::Document>,
    query_text: &str,
    exclude_file: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let model = client.embedding_model(model_name);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve(&index, query_text, exclude_file, cancel_token).await
}

#[allow(dead_code)]
async fn retrieve_fastembed(
    collection: &Collection<bson::Document>,
    query_text: &str,
    exclude_file: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let fastembed_client = rig_fastembed::Client::new();
    let model = fastembed_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve(&index, query_text, exclude_file, cancel_token).await
}

#[allow(dead_code)]
async fn do_retrieve<I: VectorStoreIndex>(
    index: &I,
    query_text: &str,
    exclude_file: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let request: VectorSearchRequest<I::Filter> = VectorSearchRequest::builder()
        .query(query_text)
        .samples((TOP_K + 2) as u64)
        .build()?;

    let results: Vec<(f64, String, String)> = tokio::select! {
        r = index.top_n(request) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG retrieval");
        }
    };

    let mut context = String::from("=== Related Context from Other Documents ===\n\n");
    let mut chars_used = context.len();
    let mut count = 0usize;

    for (score, id, text) in results {
        // Skip chunks from the same file
        if id.starts_with(exclude_file) {
            continue;
        }
        if count >= TOP_K {
            break;
        }

        let entry = format!("--- [{id}] (relevance: {score:.2}) ---\n{text}\n\n");
        if chars_used + entry.len() > MAX_CONTEXT_CHARS {
            break;
        }

        context.push_str(&entry);
        chars_used += entry.len();
        count += 1;
    }

    if count == 0 {
        return Ok(String::new());
    }

    Ok(context)
}

// ─────────────────────────────────────────────
// Query building
// ─────────────────────────────────────────────

/// Build a short representative query from a file's raw text for RAG retrieval.
///
/// Takes headings + key lines from the first ~200 lines plus a text excerpt.
#[allow(dead_code)]
pub fn build_query_text(raw_text: &str, file_name: &str) -> String {
    let mut query = format!("Document: {}\n", file_name);

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

    if query.len() > 1500 {
        query.truncate(1500);
    }

    query
}

// ─────────────────────────────────────────────
// Orphan cleanup
// ─────────────────────────────────────────────

/// Remove chunks from MongoDB whose source file is no longer in the project.
#[instrument(skip_all)]
pub async fn cleanup_orphaned_chunks(
    collection: &Collection<bson::Document>,
    active_file_names: &[String],
) -> Result<()> {
    if active_file_names.is_empty() {
        return Ok(());
    }

    // Build a regex that matches any of the active file prefixes
    let pattern = active_file_names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");

    let filter = doc! {
        "_id": { "$not": { "$regex": &pattern } }
    };

    let result = collection.delete_many(filter).await?;
    if result.deleted_count > 0 {
        info!("RAG: cleaned up {} orphaned chunks", result.deleted_count);
    }
    Ok(())
}

// ─────────────────────────────────────────────
// Combined retrieval (chunks + memories)
// ─────────────────────────────────────────────

/// Retrieve cross-file context from both the `chunks` collection (raw file
/// embeddings) and the `memories` collection (historical factoids), merging
/// both into a single context string.
#[instrument(skip_all)]
#[allow(dead_code)]
pub async fn retrieve_full_context(
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    query_text: &str,
    exclude_file: &str,
    cancel_token: &CancellationToken,
) -> String {
    let chunks_coll = chunks_collection(mongo_client);
    let chunk_ctx = retrieve_context(provider, &chunks_coll, query_text, exclude_file, cancel_token).await;

    let mem_coll = crate::memory::memories_collection(mongo_client);
    let mem_ctx = crate::memory::retrieve_memories(provider, &mem_coll, query_text, cancel_token).await;

    if chunk_ctx.is_empty() && mem_ctx.is_empty() {
        return String::new();
    }

    let mut combined = chunk_ctx;
    if !mem_ctx.is_empty() {
        combined.push_str(&mem_ctx);
    }
    combined
}

// ─────────────────────────────────────────────
// Shared vector store index (for dynamic_context)
// ─────────────────────────────────────────────

/// A cloneable, type-erased vector store index for use with rig-core's
/// `dynamic_context()` agent builder method.
///
/// Wraps an `Arc` so the same index can be shared across multiple
/// agent builds without re-creating the MongoDB index handle each time.
#[derive(Clone)]
pub struct SharedVectorIndex(Arc<dyn VectorStoreIndexDyn + Send + Sync>);

impl std::fmt::Debug for SharedVectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedVectorIndex").finish_non_exhaustive()
    }
}

impl SharedVectorIndex {
    pub fn new(index: impl VectorStoreIndexDyn + Send + Sync + 'static) -> Self {
        Self(Arc::new(index))
    }
}

impl VectorStoreIndexDyn for SharedVectorIndex {
    fn top_n<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> Pin<Box<dyn Future<Output = TopNResults> + Send + 'a>> {
        self.0.top_n(req)
    }

    fn top_n_ids<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + Send + 'a>>
    {
        self.0.top_n_ids(req)
    }
}

/// Create shared, cloneable vector store indexes for both the `chunks` and
/// `memories` collections.  The returned indexes are suitable for rig-core's
/// `.dynamic_context()` on agent builders.
///
/// Index creation may fail if the search index isn't queryable yet — failed
/// indexes are silently omitted (the caller treats an empty vec as "no RAG").
#[instrument(skip_all)]
pub async fn create_dynamic_indexes(
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
) -> Vec<SharedVectorIndex> {
    let mut indexes = Vec::new();

    // Chunks index (top-4 cross-file chunks)
    let chunks_coll = chunks_collection(mongo_client);
    match create_index_for_provider(provider, chunks_coll, "vector_index").await {
        Ok(idx) => indexes.push(idx),
        Err(e) => warn!("Failed to create chunks dynamic index: {e}"),
    }

    // Memories index (top-3 historical factoids)
    let mem_coll = crate::memory::memories_collection(mongo_client);
    match create_index_for_provider(provider, mem_coll, "memory_vector_index").await {
        Ok(idx) => indexes.push(idx),
        Err(e) => warn!("Failed to create memories dynamic index: {e}"),
    }

    indexes
}

async fn create_index_for_provider(
    provider: &EmbeddingProvider,
    collection: Collection<bson::Document>,
    index_name: &str,
) -> Result<SharedVectorIndex> {
    match provider {
        EmbeddingProvider::Ollama { client, model } => {
            let model = client.embedding_model(model);
            let index =
                MongoDbVectorIndex::new(collection, model, index_name, SearchParams::new())
                    .await?;
            Ok(SharedVectorIndex::new(index))
        }
        EmbeddingProvider::FastEmbed => {
            let fe_client = rig_fastembed::Client::new();
            let model = fe_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
            let index =
                MongoDbVectorIndex::new(collection, model, index_name, SearchParams::new())
                    .await?;
            Ok(SharedVectorIndex::new(index))
        }
    }
}
