//! Persistent cross-session memory — stores LLM-generated factoids as
//! embeddings in MongoDB so that knowledge accumulates across runs.
//!
//! File-chunk embeddings (in the `chunks` collection) capture raw document
//! content.  This module adds a separate `memories` collection for higher-level
//! factoids distilled from generated Gherkin output.

use anyhow::Result;
use mongodb::bson::{self, doc};
use mongodb::{Client as MongoClient, Collection};
use rig::client::EmbeddingsClient;
use rig::client::CompletionClient;
use rig::embeddings::EmbeddingsBuilder;
use rig::vector_store::{VectorSearchRequest, VectorStoreIndex};
use rig::Embed;
use rig_fastembed::FastembedModel;
use rig_mongodb::{MongoDbVectorIndex, SearchParams};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::rag::EmbeddingProvider;

/// A persistent memory entry — a summarized factoid from a previous LLM run.
#[derive(Embed, Clone, Serialize, Deserialize, Debug)]
pub struct ProjectMemory {
    pub id: String,
    /// Identifies the processing run (e.g. nanoid).
    pub run_id: String,
    /// Source file path that produced this factoid, or "summary".
    pub source: String,
    /// The embeddable text.
    #[embed]
    pub memory: String,
    /// Unix timestamp when this memory was created.
    pub created_at: i64,
}

/// How many historical factoids to retrieve per query.
const MEMORY_TOP_K: usize = 3;

// ─────────────────────────────────────────────
// Collection helpers
// ─────────────────────────────────────────────

/// Get the `memories` collection from the `dockock` database.
pub fn memories_collection(client: &MongoClient) -> Collection<bson::Document> {
    client.database("dockock").collection("memories")
}

// ─────────────────────────────────────────────
// Factoid extraction & storage
// ─────────────────────────────────────────────

/// Preamble for the factoid-extraction prompt.
const FACTOID_PREAMBLE: &str = "\
You are a domain analyst. Given Gherkin feature files, extract the key factoids: \
domain entities, business rules, system boundaries, integration points, and recurring patterns.\n\
Return ONLY a numbered list of concise factoid strings. One factoid per line.\n\
Do NOT include any commentary, headings, or markdown formatting. Just the numbered list.";

/// Extract factoids from generated Gherkin outputs and store them as persistent
/// memories in MongoDB with embeddings.
///
/// This runs after each processing pass so that subsequent runs can retrieve
/// cross-run insights alongside raw file-chunk context.
#[instrument(skip_all, fields(output_count, factoid_count))]
pub async fn extract_and_store_factoids(
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    generated_outputs: &[(String, String)], // (file_name, gherkin_text)
    cancel_token: &CancellationToken,
) -> Result<usize> {
    if generated_outputs.is_empty() {
        return Ok(0);
    }
    tracing::Span::current().record("output_count", generated_outputs.len());

    // Build a combined summary of all generated Gherkin for the LLM to analyze
    let combined = generated_outputs
        .iter()
        .map(|(name, text)| format!("## {name}\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    // Truncate to avoid exceeding context window
    let combined = if combined.len() > 12_000 {
        combined[..12_000].to_string()
    } else {
        combined
    };

    // Use the same Ollama instance that handles embeddings (or a fallback)
    // to extract factoids via a simple completion prompt.
    let factoids = match provider {
        EmbeddingProvider::Ollama { client, .. } => {
            extract_factoids_ollama(client, &combined, cancel_token).await?
        }
        EmbeddingProvider::FastEmbed => {
            // FastEmbed is embedding-only — no LLM available.
            // Use a simple heuristic extraction instead.
            extract_factoids_heuristic(&combined)
        }
    };

    if factoids.is_empty() {
        info!("No factoids extracted — skipping memory storage");
        return Ok(0);
    }

    let count = factoids.len();
    tracing::Span::current().record("factoid_count", count);

    // Create ProjectMemory structs from the extracted factoids
    let run_id = nanoid::nanoid!(6);
    let now = chrono::Utc::now().timestamp();

    let memories: Vec<ProjectMemory> = factoids
        .into_iter()
        .map(|text| ProjectMemory {
            id: nanoid::nanoid!(10),
            run_id: run_id.clone(),
            source: "summary".to_string(),
            memory: text,
            created_at: now,
        })
        .collect();

    // Embed and store in MongoDB
    let collection = memories_collection(mongo_client);
    match provider {
        EmbeddingProvider::Ollama { client, model } => {
            store_memories_ollama(client, model, &memories, &collection, cancel_token).await?;
        }
        EmbeddingProvider::FastEmbed => {
            store_memories_fastembed(&memories, &collection, cancel_token).await?;
        }
    }

    info!("Saved {count} factoid memories to MongoDB (run_id={run_id})");
    Ok(count)
}

/// Extract factoids using an Ollama completion model.
async fn extract_factoids_ollama(
    client: &rig::providers::ollama::Client,
    combined_gherkin: &str,
    cancel_token: &CancellationToken,
) -> Result<Vec<String>> {
    use rig::completion::Prompt;

    // Build an agent with the factoid extraction preamble
    let agent = client
        .agent("llama3.2")
        .preamble(FACTOID_PREAMBLE)
        .build();

    let response: String = tokio::select! {
        r = agent.prompt(combined_gherkin) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during factoid extraction");
        }
    };

    Ok(parse_factoid_list(&response))
}

/// Heuristic factoid extraction when no LLM is available (FastEmbed-only mode).
/// Extracts Feature/Scenario names and Given/When/Then steps as pseudo-factoids.
fn extract_factoids_heuristic(combined_gherkin: &str) -> Vec<String> {
    let mut factoids = Vec::new();
    for line in combined_gherkin.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Feature:")
            || trimmed.starts_with("Scenario:")
            || trimmed.starts_with("Scenario Outline:")
        {
            factoids.push(trimmed.to_string());
        }
    }
    // Limit to avoid noise
    factoids.truncate(30);
    factoids
}

/// Parse a numbered list response from the LLM into individual factoid strings.
fn parse_factoid_list(response: &str) -> Vec<String> {
    response
        .lines()
        .map(|line| {
            // Strip leading numbering like "1. ", "2) ", "- ", etc.
            let trimmed = line.trim();
            let stripped = trimmed
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == '-')
                .trim();
            stripped.to_string()
        })
        .filter(|s| !s.is_empty() && s.len() > 5) // skip trivially short
        .take(30) // cap max factoids per run
        .collect()
}

// ─────────────────────────────────────────────
// Memory embedding & storage
// ─────────────────────────────────────────────

/// Embed and store memories using Ollama.
async fn store_memories_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    memories: &[ProjectMemory],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<()> {
    let model = client.embedding_model(model_name);
    let texts: Vec<String> = memories.iter().map(|m| m.memory.clone()).collect();

    let embeddings = tokio::select! {
        result = EmbeddingsBuilder::new(model)
            .documents(texts)?
            .build() => {
            result?
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during memory embedding");
        }
    };

    upsert_memories(memories, &embeddings, collection).await
}

/// Embed and store memories using FastEmbed.
async fn store_memories_fastembed(
    memories: &[ProjectMemory],
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<()> {
    let fastembed_client = rig_fastembed::Client::new();
    let model = fastembed_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let texts: Vec<String> = memories.iter().map(|m| m.memory.clone()).collect();

    let embeddings = tokio::select! {
        result = EmbeddingsBuilder::new(model)
            .documents(texts)?
            .build() => {
            result?
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during memory embedding");
        }
    };

    upsert_memories(memories, &embeddings, collection).await
}

/// Write memory documents with their embedding vectors to MongoDB.
async fn upsert_memories(
    memories: &[ProjectMemory],
    embeddings: &[(String, rig::one_or_many::OneOrMany<rig::embeddings::Embedding>)],
    collection: &Collection<bson::Document>,
) -> Result<()> {
    // `embeddings` is Vec<(String, OneOrMany<Embedding>)> — the String is the
    // original memory text.  We zip with `memories` to get all metadata fields.
    for (mem, (_text, emb_set)) in memories.iter().zip(embeddings.iter()) {
        let embedding = emb_set.first_ref();
        let d = doc! {
            "_id": &mem.id,
            "run_id": &mem.run_id,
            "source": &mem.source,
            "memory": &mem.memory,
            "created_at": mem.created_at,
            "embedding": &embedding.vec,
        };
        collection
            .replace_one(doc! { "_id": &mem.id }, d)
            .upsert(true)
            .await?;
    }
    Ok(())
}

// ─────────────────────────────────────────────
// Memory retrieval
// ─────────────────────────────────────────────

/// Retrieve historical factoid memories relevant to a query.
///
/// Returns a formatted string of historical insights, or empty string on failure.
#[instrument(skip_all, fields(result_count))]
pub async fn retrieve_memories(
    provider: &EmbeddingProvider,
    collection: &Collection<bson::Document>,
    query_text: &str,
    cancel_token: &CancellationToken,
) -> String {
    let result = match provider {
        EmbeddingProvider::Ollama { client, model } => {
            retrieve_memories_ollama(client, model, collection, query_text, cancel_token).await
        }
        EmbeddingProvider::FastEmbed => {
            retrieve_memories_fastembed(collection, query_text, cancel_token).await
        }
    };

    match result {
        Ok(ctx) => {
            tracing::Span::current().record("result_count", ctx.len());
            ctx
        }
        Err(e) => {
            warn!("Memory retrieval failed: {e}");
            String::new()
        }
    }
}

async fn retrieve_memories_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    collection: &Collection<bson::Document>,
    query_text: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let model = client.embedding_model(model_name);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "memory_vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve_memories(&index, query_text, cancel_token).await
}

async fn retrieve_memories_fastembed(
    collection: &Collection<bson::Document>,
    query_text: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let fastembed_client = rig_fastembed::Client::new();
    let model = fastembed_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "memory_vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve_memories(&index, query_text, cancel_token).await
}

async fn do_retrieve_memories<I: VectorStoreIndex>(
    index: &I,
    query_text: &str,
    cancel_token: &CancellationToken,
) -> Result<String> {
    let request: VectorSearchRequest<I::Filter> = VectorSearchRequest::builder()
        .query(query_text)
        .samples(MEMORY_TOP_K as u64)
        .build()?;

    let results: Vec<(f64, String, String)> = tokio::select! {
        r = index.top_n(request) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during memory retrieval");
        }
    };

    if results.is_empty() {
        return Ok(String::new());
    }

    let mut context = String::from("\n--- Historical Memories (from previous runs) ---\n");
    for (score, _id, text) in &results {
        context.push_str(&format!("• (relevance: {score:.2}) {text}\n"));
    }

    Ok(context)
}
