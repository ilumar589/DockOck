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
// Collection configuration (multi-collection)
// ─────────────────────────────────────────────

/// Configuration for a named MongoDB collection + its vector search index.
#[derive(Debug, Clone, Copy)]
pub struct CollectionConfig {
    pub name: &'static str,
    pub index_name: &'static str,
}

pub const CHUNKS: CollectionConfig = CollectionConfig { name: "chunks", index_name: "vector_index" };
pub const MEMORIES: CollectionConfig = CollectionConfig { name: "memories", index_name: "memory_vector_index" };
pub const SCENARIOS: CollectionConfig = CollectionConfig { name: "scenarios", index_name: "scenario_vector_index" };
pub const ENTITIES: CollectionConfig = CollectionConfig { name: "entities", index_name: "entity_vector_index" };
pub const SECTIONS: CollectionConfig = CollectionConfig { name: "sections", index_name: "section_vector_index" };
pub const IMAGES: CollectionConfig = CollectionConfig { name: "images", index_name: "image_vector_index" };
pub const BUSINESS_RULES: CollectionConfig = CollectionConfig { name: "business_rules", index_name: "rule_vector_index" };
pub const CROSS_REFS: CollectionConfig = CollectionConfig { name: "cross_references", index_name: "xref_vector_index" };

/// All extended collections (excluding chunks and memories which are handled
/// by the existing `ensure_search_indexes`).
pub const EXTENDED_COLLECTIONS: &[CollectionConfig] = &[
    SCENARIOS, ENTITIES, SECTIONS, IMAGES, BUSINESS_RULES, CROSS_REFS,
];

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
// Generic collection indexing (extended collections)
// ─────────────────────────────────────────────

/// Ensure a vector search index exists for the given collection.
/// Creates the collection first if needed. This is the generic version
/// of `ensure_search_indexes` that works for any `CollectionConfig`.
#[instrument(skip_all, fields(collection = config.name, index = config.index_name))]
pub async fn ensure_collection_index(client: &MongoClient, config: &CollectionConfig, provider: &EmbeddingProvider) {
    let db = client.database("dockock");
    let dims = embedding_dimensions(provider);

    // Create collection if it doesn't exist
    let has_coll = db.list_collection_names()
        .await
        .map(|names| names.iter().any(|n| n == config.name))
        .unwrap_or(false);
    if !has_coll {
        if let Err(e) = db.create_collection(config.name).await {
            let msg = e.to_string();
            if !msg.contains("already exists") {
                warn!("Failed to create collection '{}': {e}", config.name);
            }
        }
    }

    let cmd = doc! {
        "createSearchIndexes": config.name,
        "indexes": [{
            "name": config.index_name,
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
    match db.run_command(cmd).await {
        Ok(_) => {
            info!("Created {} on {} (dims={dims})", config.index_name, config.name);
        }
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains("already exists") && !msg.contains("Duplicate") {
                warn!("Failed to create {} on {}: {e}", config.index_name, config.name);
            }
        }
    }
}

/// Ensure vector search indexes exist for all extended collections.
#[instrument(skip_all)]
pub async fn ensure_extended_indexes(client: &MongoClient, provider: &EmbeddingProvider) {
    for config in EXTENDED_COLLECTIONS {
        ensure_collection_index(client, config, provider).await;
    }
}

/// Get a named collection from the `dockock` database.
pub fn get_collection(client: &MongoClient, config: &CollectionConfig) -> Collection<bson::Document> {
    client.database("dockock").collection(config.name)
}

/// Generic function to embed a list of `(id, text, metadata_doc)` tuples
/// and upsert them into the given collection. The `metadata_doc` is merged
/// with the `_id`, `text`, and `embedding` fields.
///
/// This is the reusable core for all extended indexing functions.
#[instrument(skip_all, fields(collection = config.name, doc_count = items.len()))]
pub async fn index_documents(
    provider: &EmbeddingProvider,
    config: &CollectionConfig,
    items: &[(String, String, bson::Document)], // (id, text, extra metadata)
    client: &MongoClient,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),
) -> Result<usize> {
    if items.is_empty() {
        return Ok(0);
    }

    let collection = get_collection(client, config);

    // Check for already-indexed documents
    let all_ids: Vec<String> = items.iter().map(|(id, _, _)| id.clone()).collect();
    let existing = existing_ids(&collection, &all_ids).await;
    let new_items: Vec<&(String, String, bson::Document)> = items
        .iter()
        .filter(|(id, _, _)| !existing.contains(id))
        .collect();

    if new_items.is_empty() {
        on_progress(&format!("✅ All {} {} already indexed", items.len(), config.name));
        return Ok(0);
    }

    on_progress(&format!(
        "🔨 Indexing {} new {} ({} cached)…",
        new_items.len(), config.name, existing.len()
    ));

    let total_batches = (new_items.len() + EMBED_BATCH_SIZE - 1) / EMBED_BATCH_SIZE;
    let mut indexed = 0usize;

    for (batch_idx, batch) in new_items.chunks(EMBED_BATCH_SIZE).enumerate() {
        if cancel_token.is_cancelled() {
            anyhow::bail!("Cancelled during {} indexing", config.name);
        }

        let texts: Vec<String> = batch.iter().map(|(_, text, _)| text.clone()).collect();
        on_progress(&format!(
            "🧠 Embedding {} batch {}/{}…",
            config.name, batch_idx + 1, total_batches
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
            warn!("Embedding failed for {} batch {}", config.name, batch_idx + 1);
            return Ok(indexed);
        };

        // Upsert with metadata
        for ((id, _text, metadata), (_emb_text, emb_set)) in batch.iter().zip(embeddings.iter()) {
            let embedding = emb_set.first_ref();
            let mut d = metadata.clone();
            d.insert("_id", id.as_str());
            d.insert("embedding", &embedding.vec);
            // Ensure "text" field is present
            if d.get("text").is_none() {
                d.insert("text", _text.as_str());
            }
            collection
                .replace_one(doc! { "_id": id.as_str() }, d)
                .upsert(true)
                .await?;
        }

        indexed += batch.len();
    }

    info!("Indexed {} new documents into {}", indexed, config.name);
    Ok(indexed)
}

/// Remove documents from a collection whose `source_file` is not in the
/// active set. Works for any extended collection.
#[instrument(skip_all, fields(collection = config.name))]
pub async fn cleanup_stale_documents(
    client: &MongoClient,
    config: &CollectionConfig,
    active_source_files: &[String],
) -> Result<u64> {
    if active_source_files.is_empty() {
        return Ok(0);
    }
    let collection = get_collection(client, config);
    let filter = doc! {
        "source_file": { "$nin": active_source_files }
    };
    let result = collection.delete_many(filter).await?;
    if result.deleted_count > 0 {
        info!("Cleaned up {} stale documents from {}", result.deleted_count, config.name);
    }
    Ok(result.deleted_count)
}

// ─────────────────────────────────────────────
// Scenario indexing (Phase 2)
// ─────────────────────────────────────────────

/// Index generated Gherkin scenarios into the `scenarios` collection.
#[instrument(skip_all, fields(doc_count = gherkin_docs.len()))]
pub async fn index_scenarios(
    provider: &EmbeddingProvider,
    gherkin_docs: &[crate::gherkin::GherkinDocument],
    client: &MongoClient,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),
) -> Result<usize> {
    let run_id = nanoid::nanoid!(6);
    let now = chrono::Utc::now().timestamp();
    let mut items: Vec<(String, String, bson::Document)> = Vec::new();

    for gdoc in gherkin_docs {
        for scenario in &gdoc.scenarios {
            let slug: String = scenario.title.chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let id = format!("{}:scenario:{}", gdoc.source_file, slug);
            let text = scenario_to_text(gdoc, scenario);
            let keywords_used: Vec<String> = scenario.steps.iter()
                .map(|s| s.keyword.as_str().to_string())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            let metadata = doc! {
                "text": &text,
                "source_file": &gdoc.source_file,
                "feature_title": &gdoc.feature_title,
                "scenario_title": &scenario.title,
                "is_outline": scenario.is_outline,
                "step_count": scenario.steps.len() as i32,
                "keywords_used": &keywords_used,
                "run_id": &run_id,
                "created_at": now,
            };
            items.push((id, text, metadata));
        }
    }

    index_documents(provider, &SCENARIOS, &items, client, cancel_token, on_progress).await
}

/// Render a scenario as embeddable text.
fn scenario_to_text(doc: &crate::gherkin::GherkinDocument, scenario: &crate::gherkin::Scenario) -> String {
    let keyword = if scenario.is_outline { "Scenario Outline" } else { "Scenario" };
    let mut text = format!("Feature: {}\n  {}: {}\n", doc.feature_title, keyword, scenario.title);
    for step in &scenario.steps {
        text.push_str(&format!("    {} {}\n", step.keyword.as_str(), step.text));
    }
    text
}

// ─────────────────────────────────────────────
// Entity & Business Rule indexing (Phase 3)
// ─────────────────────────────────────────────

/// Index dependency graph nodes and their business rules.
#[instrument(skip_all, fields(graph_count = graphs.len()))]
pub async fn index_dependency_graphs(
    provider: &EmbeddingProvider,
    graphs: &[crate::depgraph::DependencyGraph],
    client: &MongoClient,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),
) -> Result<(usize, usize)> {
    let run_id = nanoid::nanoid!(6);
    let now = chrono::Utc::now().timestamp();

    let mut entity_items: Vec<(String, String, bson::Document)> = Vec::new();
    let mut rule_items: Vec<(String, String, bson::Document)> = Vec::new();

    for graph in graphs {
        let source_file = graph.source_files.first().cloned().unwrap_or_default();

        for node in &graph.nodes {
            let id = format!("{}:entity:{}", source_file, node.id);
            let text = node.to_embeddable_text();
            let state_names: Vec<String> = node.states.iter().map(|s| s.name.clone()).collect();
            let rule_ids: Vec<String> = node.rules.iter().map(|r| r.id.clone()).collect();

            let metadata = doc! {
                "text": &text,
                "source_file": &source_file,
                "entity_name": &node.name,
                "entity_type": node.entity_type.to_string(),
                "state_names": &state_names,
                "transition_count": node.transitions.len() as i32,
                "rule_count": node.rules.len() as i32,
                "rule_ids": &rule_ids,
                "run_id": &run_id,
                "created_at": now,
            };
            entity_items.push((id, text, metadata));

            // Also index each business rule separately
            for rule in &node.rules {
                let rule_id = format!("{}:rule:{}", source_file, rule.id);
                let rule_text = rule.to_embeddable_text(&node.name);
                let metadata = doc! {
                    "text": &rule_text,
                    "source_file": &source_file,
                    "rule_id": &rule.id,
                    "description": &rule.description,
                    "entity_name": &node.name,
                    "category": format!("{:?}", rule.category),
                    "lifecycle_phases": &rule.lifecycle_phases,
                    "run_id": &run_id,
                    "created_at": now,
                };
                rule_items.push((rule_id, rule_text, metadata));
            }
        }
    }

    let entity_count = index_documents(
        provider, &ENTITIES, &entity_items, client, cancel_token, &on_progress,
    ).await?;

    let rule_count = index_documents(
        provider, &BUSINESS_RULES, &rule_items, client, cancel_token, &on_progress,
    ).await?;

    Ok((entity_count, rule_count))
}

// ─────────────────────────────────────────────
// Markdown section indexing (Phase 4)
// ─────────────────────────────────────────────

/// Index markdown knowledge-base sections and cross-references.
#[instrument(skip_all, fields(doc_count = docs.len()))]
pub async fn index_markdown_sections(
    provider: &EmbeddingProvider,
    docs: &[(String, crate::markdown::MarkdownDocument)], // (source_file, doc)
    client: &MongoClient,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),
) -> Result<(usize, usize)> {
    let run_id = nanoid::nanoid!(6);
    let now = chrono::Utc::now().timestamp();

    let mut section_items: Vec<(String, String, bson::Document)> = Vec::new();
    let mut xref_items: Vec<(String, String, bson::Document)> = Vec::new();

    for (source_file, md_doc) in docs {
        // Flatten sections recursively
        let flat = crate::markdown::flatten_sections(&md_doc.sections);
        for (idx, (heading, kind_str, body, parent_heading, depth)) in flat.iter().enumerate() {
            let slug: String = heading.chars()
                .map(|c| if c.is_alphanumeric() { c } else { '_' })
                .collect();
            let text = format!("## {}\n\n{}", heading, body);

            // If section text exceeds chunk size, split into sub-chunks
            if text.len() > CHUNK_SIZE_CHARS {
                let sub_chunks = chunk_text(&text, source_file, "Markdown");
                for chunk in &sub_chunks {
                    let id = format!("{}:section:{}:{}", source_file, slug, chunk.chunk_index);
                    let metadata = doc! {
                        "text": &chunk.text,
                        "source_file": source_file.as_str(),
                        "document_title": &md_doc.title,
                        "section_heading": heading.as_str(),
                        "section_kind": kind_str.as_str(),
                        "depth": *depth as i32,
                        "parent_heading": parent_heading.as_deref(),
                        "char_count": chunk.text.len() as i32,
                        "run_id": &run_id,
                        "created_at": now,
                    };
                    section_items.push((id, chunk.text.clone(), metadata));
                }
            } else {
                let id = format!("{}:section:{}:{}", source_file, slug, idx);
                let metadata = doc! {
                    "text": &text,
                    "source_file": source_file.as_str(),
                    "document_title": &md_doc.title,
                    "section_heading": heading.as_str(),
                    "section_kind": kind_str.as_str(),
                    "depth": *depth as i32,
                    "parent_heading": parent_heading.as_deref(),
                    "char_count": text.len() as i32,
                    "run_id": &run_id,
                    "created_at": now,
                };
                section_items.push((id, text, metadata));
            }
        }

        // Cross-references
        for xref in &md_doc.cross_references {
            let id = format!("{}:xref:{}:{}", source_file, xref.target_document,
                xref.relationship.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect::<String>());
            let text = format!(
                "{} ({}) references {} for: {}",
                source_file, md_doc.title, xref.target_document, xref.description
            );
            let metadata = doc! {
                "text": &text,
                "source_file": source_file.as_str(),
                "target_file": &xref.target_document,
                "reference_type": &xref.relationship,
                "description": &xref.description,
                "run_id": &run_id,
                "created_at": now,
            };
            xref_items.push((id, text, metadata));
        }
    }

    let section_count = index_documents(
        provider, &SECTIONS, &section_items, client, cancel_token, &on_progress,
    ).await?;

    let xref_count = index_documents(
        provider, &CROSS_REFS, &xref_items, client, cancel_token, &on_progress,
    ).await?;

    Ok((section_count, xref_count))
}

// ─────────────────────────────────────────────
// Image description indexing (Phase 5)
// ─────────────────────────────────────────────

/// An image description ready for indexing.
pub struct ImageDescription {
    pub source_file: String,
    pub image_index: usize,
    pub mime_type: String,
    pub alt_text: String,
    pub description: String,
}

/// Index vision-extracted image descriptions.
#[instrument(skip_all, fields(image_count = images.len()))]
pub async fn index_image_descriptions(
    provider: &EmbeddingProvider,
    images: &[ImageDescription],
    client: &MongoClient,
    cancel_token: &CancellationToken,
    on_progress: impl Fn(&str),
) -> Result<usize> {
    let run_id = nanoid::nanoid!(6);
    let now = chrono::Utc::now().timestamp();
    let mut items: Vec<(String, String, bson::Document)> = Vec::new();

    for img in images {
        let id = format!("{}:image:{}", img.source_file, img.image_index);
        let desc_lower = img.description.to_lowercase();
        let has_reviewer = desc_lower.contains("reviewer") || desc_lower.contains("comment");
        let has_diagram = desc_lower.contains("diagram") || desc_lower.contains("flow")
            || desc_lower.contains("architecture");

        let metadata = doc! {
            "text": &img.description,
            "source_file": &img.source_file,
            "image_index": img.image_index as i32,
            "mime_type": &img.mime_type,
            "alt_text": &img.alt_text,
            "has_reviewer_comments": has_reviewer,
            "has_diagram_content": has_diagram,
            "run_id": &run_id,
            "created_at": now,
        };
        items.push((id, img.description.clone(), metadata));
    }

    index_documents(provider, &IMAGES, &items, client, cancel_token, on_progress).await
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

/// Retrieve context from extended collections (scenarios, entities, rules,
/// sections, images) alongside chunks and memories, merging all results
/// into a single context string. Used by Chat and MCP for full visibility.
#[instrument(skip_all, fields(collections))]
pub async fn retrieve_extended_context(
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    query_text: &str,
    exclude_file: Option<&str>,
    cancel_token: &CancellationToken,
) -> String {
    // Start with chunk + memory context
    let base = retrieve_full_context(
        provider, mongo_client, query_text,
        exclude_file.unwrap_or(""), cancel_token,
    ).await;

    // Query each extended collection for additional relevant hits
    let extended_configs: &[(&CollectionConfig, &str, usize)] = &[
        (&SCENARIOS, "Relevant Scenarios", 4),
        (&ENTITIES, "Relevant Entities", 3),
        (&BUSINESS_RULES, "Relevant Business Rules", 3),
        (&SECTIONS, "Relevant Knowledge Base Sections", 4),
        (&IMAGES, "Relevant Image Descriptions", 2),
    ];

    let mut extra = String::new();
    let mut chars_left = MAX_CONTEXT_CHARS.saturating_sub(base.len());

    for (config, header, top_k) in extended_configs {
        if chars_left < 200 || cancel_token.is_cancelled() {
            break;
        }
        let collection = get_collection(mongo_client, config);
        let hits = retrieve_from_any_collection(
            provider, &collection, config.index_name, query_text, *top_k, cancel_token,
        ).await;

        if !hits.is_empty() {
            let section_header = format!("\n=== {} ===\n\n", header);
            extra.push_str(&section_header);
            chars_left = chars_left.saturating_sub(section_header.len());

            for (score, id, text) in &hits {
                if chars_left < 100 {
                    break;
                }
                let entry = format!("--- [{id}] (relevance: {score:.2}) ---\n{text}\n\n");
                if entry.len() > chars_left {
                    break;
                }
                extra.push_str(&entry);
                chars_left = chars_left.saturating_sub(entry.len());
            }
        }
    }

    if extra.is_empty() {
        return base;
    }

    let mut combined = base;
    combined.push_str(&extra);
    combined
}

/// Query a single collection by vector search. Returns `(score, id, text)` tuples.
async fn retrieve_from_any_collection(
    provider: &EmbeddingProvider,
    collection: &Collection<bson::Document>,
    index_name: &str,
    query_text: &str,
    top_k: usize,
    cancel_token: &CancellationToken,
) -> Vec<(f64, String, String)> {
    let result = match provider {
        EmbeddingProvider::Ollama { client, model } => {
            let model = client.embedding_model(model);
            let Ok(index) = MongoDbVectorIndex::new(collection.clone(), model, index_name, SearchParams::new()).await else {
                return Vec::new();
            };
            do_generic_retrieve(&index, query_text, top_k, cancel_token).await
        }
        EmbeddingProvider::FastEmbed => {
            let fe_client = rig_fastembed::Client::new();
            let model = fe_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
            let Ok(index) = MongoDbVectorIndex::new(collection.clone(), model, index_name, SearchParams::new()).await else {
                return Vec::new();
            };
            do_generic_retrieve(&index, query_text, top_k, cancel_token).await
        }
    };
    result.unwrap_or_default()
}

async fn do_generic_retrieve<I: VectorStoreIndex>(
    index: &I,
    query_text: &str,
    top_k: usize,
    cancel_token: &CancellationToken,
) -> Result<Vec<(f64, String, String)>> {
    let request: VectorSearchRequest<I::Filter> = VectorSearchRequest::builder()
        .query(query_text)
        .samples((top_k + 2) as u64)
        .build()?;

    let results: Vec<(f64, String, serde_json::Value)> = tokio::select! {
        r = index.top_n(request) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during extended retrieval");
        }
    };

    Ok(results.into_iter()
        .take(top_k)
        .filter_map(|(score, id, doc)| {
            let text = doc.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if text.is_empty() { None } else { Some((score, id, text)) }
        })
        .collect())
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

/// Create shared, cloneable vector store indexes for chunks, memories,
/// and all extended collections. The returned indexes are suitable for
/// rig-core's `.dynamic_context()` on agent builders.
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

    // Extended collection indexes (scenarios, entities, sections, images, rules, xrefs)
    for config in EXTENDED_COLLECTIONS {
        let coll = get_collection(mongo_client, config);
        match create_index_for_provider(provider, coll, config.index_name).await {
            Ok(idx) => indexes.push(idx),
            Err(e) => {
                // Non-fatal: extended indexes may not exist yet if no data has been indexed
                info!("Extended index {} not available yet: {e}", config.index_name);
            }
        }
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
