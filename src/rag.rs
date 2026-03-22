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
const TOP_K: usize = 8;

/// Maximum total characters of cross-file context to inject into the prompt.
const MAX_CONTEXT_CHARS: usize = 24_000;

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
    let mut search_start = len.saturating_sub(len / 5);
    // Ensure search_start falls on a UTF-8 char boundary
    while search_start < len && !text.is_char_boundary(search_start) {
        search_start += 1;
    }
    if let Some(pos) = text[search_start..].rfind('\n') {
        let end = search_start + pos + 1;
        text[..end].to_string()
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

/// Return the embedding dimensions for the active provider / model.
pub fn embedding_dimensions(provider: &EmbeddingProvider) -> i32 {
    match provider {
        EmbeddingProvider::Ollama { model, .. } => match model.as_str() {
            "mxbai-embed-large" => 1024,
            "nomic-embed-text" => 768,
            _ => 768, // safe default for unknown Ollama models
        },
        EmbeddingProvider::FastEmbed => 384,
    }
}

/// Ensure that the required Atlas Search vector indexes exist on both the
/// `chunks` and `memories` collections.  On `mongodb-atlas-local` the indexes
/// are created via `createSearchIndexes`; if they already exist the command
/// returns an error that we silently ignore.
///
/// The `numDimensions` is set dynamically based on the selected embedding
/// provider so that the index matches the actual embedding vectors.
#[instrument(skip_all)]
pub async fn ensure_search_indexes(client: &MongoClient, provider: &EmbeddingProvider) {
    let db = client.database("dockock");
    let dims = embedding_dimensions(provider);
    eprintln!("[RAG] ensure_search_indexes: dims={dims} provider={:?}", std::mem::discriminant(provider));

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
                    "numDimensions": dims,
                    "similarity": "cosine"
                }]
            }
        }]
    };
    match db.run_command(chunks_cmd).await {
        Ok(_) => {
            eprintln!("[RAG] Created vector_index on chunks (dims={dims})");
            info!("Created vector_index on chunks collection (dims={dims})");
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!("[RAG] createSearchIndexes chunks error: {msg}");
            if msg.contains("already exists") || msg.contains("Duplicate") {
                info!("vector_index already exists on chunks collection");
            } else {
                warn!("Failed to create vector_index on chunks: {e}");
            }
        }
    }

    // Index for `memories` collection — create the collection first if it
    // doesn't exist (createSearchIndexes requires the collection to exist).
    let has_memories = db.list_collection_names()
        .await
        .map(|names| names.iter().any(|n| n == "memories"))
        .unwrap_or(false);

    if !has_memories {
        eprintln!("[RAG] memories collection does not exist — creating it");
        if let Err(e) = db.create_collection("memories").await {
            eprintln!("[RAG] Failed to create memories collection: {e}");
            warn!("Failed to create memories collection: {e}");
        }
    }

    let memories_cmd = doc! {
        "createSearchIndexes": "memories",
        "indexes": [{
            "name": "memory_vector_index",
            "type": "vectorSearch",
            "definition": {
                "fields": [{
                    "type": "vector",
                    "path": "embedding",
                    "numDimensions": dims,
                    "similarity": "cosine"
                }]
            }
        }]
    };
    match db.run_command(memories_cmd).await {
        Ok(_) => {
            eprintln!("[RAG] Created memory_vector_index on memories (dims={dims})");
            info!("Created memory_vector_index on memories collection (dims={dims})");
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!("[RAG] createSearchIndexes memories error: {msg}");
            if msg.contains("already exists") || msg.contains("Duplicate") {
                info!("memory_vector_index already exists on memories collection");
            } else {
                warn!("Failed to create memory_vector_index on memories: {e}");
            }
        }
    }
}

/// Wait for a vector search index to reach READY status on the given
/// collection.  Polls `$listSearchIndexes` every 2 seconds up to `timeout`.
/// Returns `true` if the index became ready, `false` on timeout.
#[instrument(skip_all, fields(collection, index_name))]
pub async fn wait_for_search_index_ready(
    client: &MongoClient,
    collection_name: &str,
    index_name: &str,
    timeout: std::time::Duration,
    on_status: impl Fn(&str),
) -> bool {
    use futures::TryStreamExt;

    let db = client.database("dockock");
    let deadline = tokio::time::Instant::now() + timeout;

    on_status(&format!("⏳ Waiting for search index '{index_name}' to become queryable…"));
    eprintln!("[RAG] wait_for_search_index_ready: collection={collection_name} index={index_name} timeout={}s", timeout.as_secs());

    loop {
        // Use the aggregation pipeline form: db.collection.aggregate([{$listSearchIndexes:{name:...}}])
        let coll: Collection<bson::Document> = db.collection(collection_name);
        let pipeline = vec![doc! { "$listSearchIndexes": { "name": index_name } }];
        match coll.aggregate(pipeline).await {
            Ok(mut cursor) => {
                let mut found_any = false;
                while let Ok(Some(doc)) = cursor.try_next().await {
                    found_any = true;
                    eprintln!("[RAG] listSearchIndexes result: {doc:?}");
                    if let Ok(status) = doc.get_str("status") {
                        info!("Search index '{index_name}' on '{collection_name}': status={status}");
                        if status == "READY" {
                            on_status(&format!("✅ Search index '{index_name}' is ready"));
                            return true;
                        }
                    }
                    // "queryable" field is also a good signal
                    if let Ok(true) = doc.get_bool("queryable") {
                        on_status(&format!("✅ Search index '{index_name}' is queryable"));
                        return true;
                    }
                }
                if !found_any {
                    eprintln!("[RAG] listSearchIndexes returned 0 results for '{index_name}' on '{collection_name}'");
                }
            }
            Err(e) => {
                eprintln!("[RAG] listSearchIndexes aggregation failed: {e}");
            }
        }

        if tokio::time::Instant::now() >= deadline {
            warn!("Timed out waiting for search index '{index_name}' on '{collection_name}'");
            on_status(&format!("⚠ Search index '{index_name}' not ready after {}s — queries may return empty results", timeout.as_secs()));
            return false;
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
/// Chunks whose `document_id` already exists in MongoDB are skipped (incremental).
/// Embedding is done in batches of `EMBED_BATCH_SIZE` for progress visibility
/// and to avoid OOM on large projects.
///
/// Falls back gracefully: if embedding fails, returns `false` so
/// the caller can use excerpt-based context instead.
const EMBED_BATCH_SIZE: usize = 64;

#[instrument(skip_all, fields(chunk_count))]
pub async fn build_index(
    provider: &EmbeddingProvider,
    chunks: &[TextChunk],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),  // status message
) -> Result<bool> {
    if chunks.is_empty() {
        info!("RAG: no chunks to index");
        return Ok(false);
    }
    tracing::Span::current().record("chunk_count", chunks.len());

    // ── Skip chunks already in MongoDB (incremental indexing) ──
    on_progress("🔍 Checking for already-indexed chunks…");
    eprintln!("[DEBUG RAG build_index] checking existing_ids for {} chunks…", chunks.len());
    let all_ids: Vec<String> = chunks.iter().map(|c| c.document_id()).collect();
    let existing = existing_ids(collection, &all_ids).await;
    eprintln!("[DEBUG RAG build_index] existing_ids done: {} found", existing.len());
    let new_chunks: Vec<&TextChunk> = chunks
        .iter()
        .filter(|c| !existing.contains(&c.document_id()))
        .collect();

    if new_chunks.is_empty() {
        on_progress(&format!("✅ All {} chunks already indexed — skipping embed", chunks.len()));
        info!("RAG: all {} chunks already indexed — skipping", chunks.len());
        return Ok(true);
    }
    on_progress(&format!(
        "🔨 {} new chunks to embed ({} cached)", new_chunks.len(), existing.len()
    ));

    let total_batches = (new_chunks.len() + EMBED_BATCH_SIZE - 1) / EMBED_BATCH_SIZE;
    let mut indexed = 0usize;

    for (batch_idx, batch) in new_chunks.chunks(EMBED_BATCH_SIZE).enumerate() {
        if cancel_token.is_cancelled() {
            anyhow::bail!("Cancelled during RAG indexing");
        }
        let ids: Vec<String> = batch.iter().map(|c| c.document_id()).collect();
        let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();

        on_progress(&format!(
            "🧠 Embedding batch {}/{} ({} texts)…",
            batch_idx + 1, total_batches, texts.len()
        ));

        let embeddings = match provider {
            EmbeddingProvider::Ollama { client, model } => {
                embed_ollama(client, model, texts, cancel_token).await?
            }
            EmbeddingProvider::FastEmbed => {
                embed_fastembed(texts, cancel_token).await?
            }
        };

        let Some(embeddings) = embeddings else {
            return Ok(false);
        };

        on_progress(&format!(
            "💾 Storing batch {}/{} in MongoDB…", batch_idx + 1, total_batches
        ));

        upsert_embeddings(&ids, &embeddings, collection).await?;
        indexed += batch.len();
        info!(
            "RAG: batch {}/{} done ({}/{} chunks indexed)",
            batch_idx + 1,
            total_batches,
            indexed,
            new_chunks.len()
        );
    }

    info!(
        "RAG index built via {} ({} new chunks, {} already existed)",
        match provider {
            EmbeddingProvider::Ollama { .. } => "Ollama",
            EmbeddingProvider::FastEmbed => "FastEmbed",
        },
        indexed,
        existing.len()
    );
    Ok(true)
}

/// Return the set of IDs that already exist in the collection.
async fn existing_ids(
    collection: &Collection<bson::Document>,
    ids: &[String],
) -> std::collections::HashSet<String> {
    use futures::TryStreamExt;
    let mut found = std::collections::HashSet::new();
    // Query in batches of 500 to avoid oversized $in arrays.
    // Timeout after 10s total — if MongoDB is slow we just re-embed.
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for batch in ids.chunks(500) {
            let filter = doc! { "_id": { "$in": batch } };
            if let Ok(mut cursor) = collection
                .find(filter)
                .projection(doc! { "_id": 1 })
                .await
            {
                while let Ok(Some(d)) = cursor.try_next().await {
                    if let Ok(id) = d.get_str("_id") {
                        found.insert(id.to_string());
                    }
                }
            }
        }
    })
    .await;
    if result.is_err() {
        warn!("RAG: existing_ids check timed out — will re-embed all chunks");
        return std::collections::HashSet::new();
    }
    found
}

type EmbedResult = Vec<(String, rig::one_or_many::OneOrMany<rig::embeddings::Embedding>)>;

/// Embed a batch of texts via Ollama. Returns `None` if embedding fails.
/// Times out after 120 seconds per batch to avoid silent hangs.
async fn embed_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    texts: Vec<String>,
    cancel_token: &CancellationToken,
) -> Result<Option<EmbedResult>> {
    let model = client.embedding_model(model_name);
    let builder = EmbeddingsBuilder::new(model).documents(texts)?;
    let embeddings = tokio::select! {
        result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            builder.build(),
        ) => {
            match result {
                Ok(Ok(e)) => e,
                Ok(Err(e)) => {
                    warn!("RAG: Ollama embedding failed: {e}");
                    return Ok(None);
                }
                Err(_) => {
                    warn!("RAG: Ollama embedding timed out (120s)");
                    return Ok(None);
                }
            }
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    };
    Ok(Some(embeddings))
}

/// Embed a batch of texts via FastEmbed. Returns `None` if embedding fails.
/// Times out after 300 seconds per batch (CPU can be slow).
async fn embed_fastembed(
    texts: Vec<String>,
    cancel_token: &CancellationToken,
) -> Result<Option<EmbedResult>> {
    let fastembed_client = rig_fastembed::Client::new();
    let model = fastembed_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let builder = EmbeddingsBuilder::new(model).documents(texts)?;
    let embeddings = tokio::select! {
        result = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            builder.build(),
        ) => {
            match result {
                Ok(Ok(e)) => e,
                Ok(Err(e)) => {
                    warn!("RAG: FastEmbed embedding failed: {e}");
                    return Ok(None);
                }
                Err(_) => {
                    warn!("RAG: FastEmbed embedding timed out (300s)");
                    return Ok(None);
                }
            }
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    };
    Ok(Some(embeddings))
}

/// Upsert embedded documents into MongoDB using concurrent writes.
async fn upsert_embeddings(
    ids: &[String],
    embeddings: &[(String, rig::one_or_many::OneOrMany<rig::embeddings::Embedding>)],
    collection: &Collection<bson::Document>,
) -> Result<()> {
    use futures::stream::{FuturesUnordered, StreamExt};
    let futures: FuturesUnordered<_> = ids
        .iter()
        .zip(embeddings.iter())
        .map(|(id, (text, emb_set))| {
            let id = id.clone();
            let text = text.clone();
            let embedding = emb_set.first_ref().vec.clone();
            let coll = collection.clone();
            async move {
                let d = doc! {
                    "_id": &id,
                    "text": &text,
                    "embedding": &embedding,
                };
                coll.replace_one(doc! { "_id": &id }, d)
                    .upsert(true)
                    .await
            }
        })
        .collect();
    let results: Vec<_> = futures.collect().await;
    for r in results {
        r?;
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

    let results: Vec<(f64, String, serde_json::Value)> = tokio::select! {
        r = index.top_n(request) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG retrieval");
        }
    };

    let mut context = String::from("=== Related Context from Other Documents ===\n\n");
    let mut chars_used = context.len();
    let mut count = 0usize;

    for (score, id, doc) in results {
        // Extract text from the document — field is "text" for chunks
        let text = doc.get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if text.is_empty() {
            continue;
        }
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

/// Maximum character length for embedding queries.
/// Embedding models have much smaller context windows than LLMs (typically 512-8192 tokens).
/// We truncate queries to ~2048 chars (~512 tokens) which is safe for all common models
/// and still captures enough semantic signal for similarity search.
const MAX_EMBEDDING_QUERY_CHARS: usize = 2048;

/// Truncate a query string to fit within embedding model context limits,
/// snapping to a word boundary to avoid splitting mid-token.
fn truncate_query(query: &str) -> String {
    if query.len() <= MAX_EMBEDDING_QUERY_CHARS {
        return query.to_string();
    }
    let truncated = &query[..MAX_EMBEDDING_QUERY_CHARS];
    // Snap to last whitespace to avoid cutting mid-word
    if let Some(pos) = truncated.rfind(char::is_whitespace) {
        truncated[..pos].to_string()
    } else {
        truncated.to_string()
    }
}

/// Rebuild a VectorSearchRequest with a truncated query to prevent
/// embedding model context overflow.
fn truncate_request(
    req: VectorSearchRequest<Filter<serde_json::Value>>,
) -> VectorSearchRequest<Filter<serde_json::Value>> {
    let query = req.query();
    if query.len() <= MAX_EMBEDDING_QUERY_CHARS {
        return req;
    }
    let short_query = truncate_query(query);
    let mut builder = VectorSearchRequest::builder()
        .query(short_query)
        .samples(req.samples());
    if let Some(t) = req.threshold() {
        builder = builder.threshold(t);
    }
    if let Some(f) = req.filter().clone() {
        builder = builder.filter(f);
    }
    builder.build().unwrap_or(req)
}

impl VectorStoreIndexDyn for SharedVectorIndex {
    fn top_n<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> Pin<Box<dyn Future<Output = TopNResults> + Send + 'a>> {
        self.0.top_n(truncate_request(req))
    }

    fn top_n_ids<'a>(
        &'a self,
        req: VectorSearchRequest<Filter<serde_json::Value>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(f64, String)>, VectorStoreError>> + Send + 'a>>
    {
        self.0.top_n_ids(truncate_request(req))
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
