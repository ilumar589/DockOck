# RAG Cross-File Context & Observability — Feature Plan

> **Scope**: Replace the excerpt-based cross-file context mechanism with semantic
> RAG using rig-core's embedding/vector-store APIs, and add structured
> observability via the `tracing` + OpenTelemetry ecosystem.
>
> **Backwards compatibility**: Not required — full rewrite of `rag.rs`,
> `context.rs`, and supporting call-sites is acceptable.
>
> **Constraint**: All async work must respect the existing CancellationToken /
> TaskTracker structured-concurrency patterns
> (see `docs/CANCELLATION_AND_STRUCTURED_CONCURRENCY.md`).

---

## 1  Current State

### 1.1  Context Sharing Today

| Component | Role | Limitation |
|-----------|------|------------|
| `ProjectContext` (`context.rs`) | Stores raw `FileContent` map, extracted entities, notes | Context = first 400 chars per file — lossy, no semantic relevance |
| `build_summary()` / `build_summary_excluding()` | Formats excerpt-based cross-file context string | Fixed-size excerpts miss critical detail buried later in a document |
| `build_glossary()` | Formats entity list from heuristic extraction | Heuristic-only; no embedding-based entity linking |
| `rag.rs` | **Dead code** — TF-IDF hash vectoriser + brute-force cosine search | Disabled ("runtime hangs"); homebrew embeddings have no semantic quality |
| LLM pipeline (`llm/mod.rs`) | `process_file` / `process_group` accept `rag_context: Option<&str>`, always `None` | RAG path is never exercised |

### 1.2  Observability Today

- `tracing` and `tracing-subscriber` are in Cargo.toml but only used for basic
  `info!()` logging in a handful of places.
- No structured spans, no `#[instrument]`, no OpenTelemetry export.
- LLM call latency, token usage, retry counts, and cache hit/miss rates are
  invisible.

---

## 2  Design: RAG Cross-File Context

### 2.1  Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                    process_files()                       │
│                                                         │
│  Phase 1   Parse files → FileContent map                │
│  Phase 1.25 Extract entities (heuristic, keep as-is)    │
│  Phase 1.3  ★ NEW: Build RAG index                      │
│             ┌──────────────────────────────────┐        │
│             │  for each FileContent:           │        │
│             │    chunk → embed → upsert into   │        │
│             │    MongoDB (via rig-mongodb)      │        │
│             └──────────────────────────────────┘        │
│  Phase 1.35 Prime KV-cache (glossary, as now)           │
│  Phase 2   Per-file/group tasks (parallel, semaphore)   │
│             ┌──────────────────────────────────┐        │
│             │  Before LLM call:                │        │
│             │    query = build_query(raw_text)  │        │
│             │    rag_ctx = index.top_n(query,4) │        │
│             │    → inject into LLM prompt       │        │
│             └──────────────────────────────────┘        │
└─────────────────────────────────────────────────────────┘

MongoDB runs as a Docker container alongside the Ollama instances
(see `docker-compose.yml`). A `dockock` database with a `chunks` collection
and a `memories` collection stores all embeddings persistently across runs.
```

### 2.2  Embedding Strategy

**Three modes**, matching existing provider support plus a zero-dependency local
option:

| Provider | Crate | Embedding Model | Dim | Notes |
|----------|-------|----------------|-----|-------|
| Ollama (local) | `rig-core` | `nomic-embed-text` (or configurable) | 768 | Free, no API key, fast on GPU. Pulled automatically if missing. |
| OpenAI (cloud) | `rig-core` | `text-embedding-3-small` | 1536 | Requires API key; low cost ($0.02/1M tokens). |
| FastEmbed (local, CPU) | `rig-fastembed` | `AllMiniLML6V2Q` (default) | 384 | Fully in-process, no external service needed. CPU-only, ~60 ms/chunk. Zero network overhead. Ideal for air-gapped environments or as fallback. |

The embedding model **must match** between indexing and querying — rig-core
enforces this via its type system (`EmbeddingModel` passed to both
`EmbeddingsBuilder` and `vector_store.index(model)`).

**Provider selection priority** (when set to "Auto"):
1. **Ollama** — if the configured Ollama instance is reachable and the
   embedding model is available.
2. **OpenAI** — if an API key is present and Ollama is unavailable.
3. **FastEmbed** — always available as a local CPU fallback. No external
   service or API key required. Uses `rig_fastembed::Client` which downloads
   the ONNX model on first use and caches it locally.

**Fallback**: If the primary embedding provider fails (e.g. Ollama is down and
no OpenAI key), automatically fall back to FastEmbed so the RAG pipeline always
runs. If even FastEmbed fails (unlikely — it's pure CPU), fall back to the
existing excerpt-based `build_summary()` approach. Log a warning via
`tracing::warn!` at each fallback step.

### 2.3  Chunking

Retain the proven chunking logic from the current dead `rag.rs`, with minor
adjustments:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| `CHUNK_SIZE_CHARS` | 1024 | ~256 tokens — good balance for embedding models |
| `CHUNK_OVERLAP_CHARS` | 256 | 25% overlap — captures boundary context |
| Snap to line boundary | Yes | Avoids splitting mid-sentence |

These values are tuned for `nomic-embed-text` (512-token context window) and
`text-embedding-3-small` (8191-token window). Smaller chunks produce more
focused embeddings.

### 2.4  Indexing (Phase 1.3)

```rust
use mongodb::{Client as MongoClient, Collection};
use mongodb::bson::{self, doc};
use mongodb::options::ClientOptions;
use rig::embeddings::EmbeddingsBuilder;
use rig_mongodb::{MongoDbVectorIndex, SearchParams};

// Inside process_files(), after Phase 1.25:

// Connect to the MongoDB instance running in Docker
let mongo_opts = ClientOptions::parse("mongodb://localhost:27017")
    .await?;
let mongo_client = MongoClient::with_options(mongo_opts)?;
let collection: Collection<bson::Document> = mongo_client
    .database("dockock")
    .collection("chunks");

let embedding_model = match &orchestrator.provider {
    Provider::Ollama { .. } => {
        ollama_client.embedding_model("nomic-embed-text")
    }
    Provider::OpenAI { .. } => {
        openai_client.embedding_model(TEXT_EMBEDDING_3_SMALL)
    }
};

// FastEmbed fallback — used when neither Ollama nor OpenAI is available,
// or when explicitly selected by the user.
// Uses rig-fastembed for fully local, CPU-only embedding (no external service).
//
//   let fastembed_client = rig_fastembed::Client::new();
//   let embedding_model = fastembed_client
//       .embedding_model(&rig_fastembed::FastembedModel::AllMiniLML6V2Q);

// Chunk all files
let chunks: Vec<TextChunk> = project_context
    .file_contents
    .values()
    .flat_map(|fc| chunk_text(&fc.raw_text, &fc.path, &fc.file_type))
    .collect();

// Embed in batches (rig-core handles batching internally)
let embeddings = EmbeddingsBuilder::new(embedding_model.clone())
    .documents(chunks.iter().map(|c| (c.document_id(), c.text.clone())).collect::<Vec<_>>())?
    .build()
    .await?;

// Upsert into MongoDB as BSON documents with embedding vectors
let mongo_documents = embeddings
    .iter()
    .map(|((id, text), embedding)| {
        doc! {
            "_id": id.clone(),
            "text": text.clone(),
            "embedding": embedding.first().vec.clone(),
        }
    })
    .collect::<Vec<_>>();

// Replace existing chunks (incremental: only changed files are re-chunked)
for d in &mongo_documents {
    collection.replace_one(doc! { "_id": d.get_str("_id").unwrap() }, d.clone())
        .upsert(true)
        .await?;
}

// Create the searchable vector index (requires an Atlas Search index
// named "vector_index" on the collection — see docker-compose setup)
let rag_index = Arc::new(
    MongoDbVectorIndex::new(
        collection.clone(),
        embedding_model.clone(),
        "vector_index",
        SearchParams::new(),
    ).await?
);
```

**Cancellation**: The `EmbeddingsBuilder::build()` call is a single await point.
Wrap in `tokio::select!` with the cancel token:

```rust
tokio::select! {
    result = EmbeddingsBuilder::new(model.clone())
        .documents(documents)?
        .build() => {
        let embeddings = result?;
        // upsert into MongoDB …
    }
    _ = cancel_token.cancelled() => {
        anyhow::bail!("Cancelled during RAG indexing");
    }
}
```

### 2.5  Retrieval (Per-File, Phase 2)

For each file being processed, build a query from its content and retrieve the
top-N most relevant chunks from **other** files:

```rust
use rig::vector_store::{VectorSearchRequest, VectorStoreIndex};

let query_text = build_query_text(&raw_text); // headings + key lines

let request = VectorSearchRequest::builder()
    .query(&query_text)
    .samples(4)                            // top-4 chunks
    .build()?;

let results = rag_index.top_n::<String>(request).await?;

// Filter out chunks from the same file, format as context string
let rag_context = results
    .into_iter()
    .filter(|(_, id, _)| !id.starts_with(&file_name))
    .take(4)
    .map(|(score, id, text)| format!(
        "--- [{id}] (relevance: {score:.2}) ---\n{text}"
    ))
    .collect::<Vec<_>>()
    .join("\n\n");
```

**Cancellation**: The `top_n` call against MongoDB is an async network round-trip.
Wrap in `tokio::select!` with the cancel token for consistency.

### 2.6  Prompt Injection

The retrieved `rag_context` string replaces the current excerpt-based
`context.build_summary()` output. It flows into the existing `generate()` /
`generate_group()` methods via the `context_summary` parameter — no signature
changes required:

```rust
// In process_file():
let context_summary = if !rag_context.is_empty() {
    rag_context
} else {
    context.build_summary() // fallback
};
```

### 2.7  `dynamic_context()` Agent Integration (Optional Enhancement)

Rig-core's agent builder supports `dynamic_context(n, vector_store)` which
automatically retrieves relevant documents and injects them into the prompt for
each call:

```rust
let agent = client
    .agent(model)
    .preamble(GENERATOR_PREAMBLE)
    .dynamic_context(4, rag_index.clone())
    .build();
```

This is a **future enhancement** — it requires restructuring how agents are
built to be per-file rather than shared (since the query differs per file).
Phase 1 uses manual retrieval + injection (§2.5–2.6).

### 2.8  Changes to `context.rs`

`ProjectContext` remains as the data container for raw file contents and
entities. The following changes apply:

- **Keep**: `file_contents`, `entities`, `notes`, `add_file()`,
  `extract_entities()`, `build_glossary()`
- **Keep**: `build_summary()` / `build_summary_excluding()` as **fallback** when
  RAG is unavailable
- **Remove**: Nothing — the struct is still useful for entity extraction and
  fallback
- **Add**: `chunk_all_files() -> Vec<TextChunk>` convenience method that returns
  all chunks for the current file contents

### 2.9  Rewrite of `rag.rs`

The current dead code in `rag.rs` (TF-IDF hashing, brute-force search, etc.) is
**deleted entirely** and replaced with:

```rust
//! RAG pipeline using rig-core embedding APIs + MongoDB vector store (via Docker).

use anyhow::Result;
use mongodb::bson::{self, doc};
use mongodb::options::ClientOptions;
use mongodb::{Client as MongoClient, Collection};
use rig::embeddings::EmbeddingsBuilder;
use rig::vector_store::VectorStoreIndex;
use rig_mongodb::{MongoDbVectorIndex, SearchParams};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

// Re-export chunk types
pub use chunk::{TextChunk, chunk_text, build_query_text};

mod chunk {
    // Chunking logic (migrated from current rag.rs, cleaned up)
}

/// Connect to the MongoDB instance running in Docker.
pub async fn connect_to_mongodb(connection_string: &str) -> Result<Collection<bson::Document>> {
    let options = ClientOptions::parse(connection_string).await?;
    let client = MongoClient::with_options(options)?;
    Ok(client.database("dockock").collection("chunks"))
}

/// Connect to the MongoDB memories collection.
pub async fn connect_to_memories(connection_string: &str) -> Result<Collection<bson::Document>> {
    let options = ClientOptions::parse(connection_string).await?;
    let client = MongoClient::with_options(options)?;
    Ok(client.database("dockock").collection("memories"))
}

/// Build the RAG index by embedding all project file chunks and upserting
/// them into MongoDB. Returns a `MongoDbVectorIndex` for querying.
///
/// Returns `None` if embedding fails (caller should fall back to excerpt
/// context).
#[instrument(skip_all, fields(file_count, chunk_count))]
pub async fn build_index<M>(
    model: M,
    files: &crate::context::ProjectContext,
    collection: &Collection<bson::Document>,
    cancel_token: &CancellationToken,
) -> Result<Option<MongoDbVectorIndex<M>>>
where
    M: rig::embeddings::EmbeddingModel + Clone,
{
    let chunks = files.chunk_all_files();
    tracing::Span::current().record("file_count", files.file_contents.len());
    tracing::Span::current().record("chunk_count", chunks.len());

    if chunks.is_empty() {
        return Ok(None);
    }

    let documents: Vec<(String, String)> = chunks
        .into_iter()
        .map(|c| (c.document_id(), c.text))
        .collect();

    let embeddings = tokio::select! {
        result = EmbeddingsBuilder::new(model.clone())
            .documents(documents)?
            .build() => { result? }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    };

    // Upsert embedded chunks into MongoDB
    let mongo_docs: Vec<bson::Document> = embeddings
        .iter()
        .map(|((id, text), embedding)| {
            doc! {
                "_id": id.clone(),
                "text": text.clone(),
                "embedding": embedding.first().vec.clone(),
            }
        })
        .collect();

    for d in &mongo_docs {
        collection
            .replace_one(
                doc! { "_id": d.get_str("_id").unwrap() },
                d.clone(),
            )
            .upsert(true)
            .await?;
    }

    info!("RAG index built and persisted to MongoDB");

    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "vector_index",
        SearchParams::new(),
    )
    .await?;

    Ok(Some(index))
}

/// Retrieve cross-file context for a given file.
#[instrument(skip_all, fields(file_name, result_count))]
pub async fn retrieve_context<I>(
    index: &I,
    query_text: &str,
    exclude_file: &str,
    top_n: usize,
) -> Result<String>
where
    I: VectorStoreIndex,
{
    let results = index.top_n::<String>(query_text, top_n + 2).await?;

    let context: String = results
        .into_iter()
        .filter(|(_, id, _)| !id.starts_with(exclude_file))
        .take(top_n)
        .map(|(score, id, text)| {
            format!("--- [{id}] (relevance: {score:.2}) ---\n{text}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(context)
}
```

---

## 3  Design: Observability

### 3.1  Goals

1. **Structured spans** around every significant operation (parsing, embedding,
   LLM calls, review, caching)
2. **Key metrics** recorded as span fields: token counts, latency, cache
   hit/miss, retry count, chunk count
3. **Configurable verbosity** via `RUST_LOG` environment variable
4. **Optional OpenTelemetry export** for production trace collection (e.g.
   Langfuse, Jaeger, Grafana Tempo)

### 3.2  Dependency Changes

Update `Cargo.toml`:

```toml
# Logging / observability
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# Optional: OpenTelemetry export (behind feature flag)
tracing-opentelemetry = { version = "0.30", optional = true }
opentelemetry = { version = "0.31", features = ["trace"], optional = true }
opentelemetry_sdk = { version = "0.31", features = ["rt-tokio"], optional = true }
opentelemetry-otlp = { version = "0.31", features = ["tonic", "trace"], optional = true }

[features]
default = []
otel = ["tracing-opentelemetry", "opentelemetry", "opentelemetry_sdk", "opentelemetry-otlp"]
```

This keeps the binary lean by default. Users who want OTEL export compile with
`cargo build --features otel`.

### 3.3  Subscriber Initialization

In `main.rs`:

```rust
fn init_tracing() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,dockock=debug,rig=info".into());

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    #[cfg(feature = "otel")]
    let registry = {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()
            .expect("OTLP exporter");
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name("dockock")
                    .build(),
            )
            .build();
        let tracer = provider.tracer("dockock");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        registry.with(otel_layer)
    };

    registry.init();
}
```

### 3.4  Instrumentation Points

The following functions get `#[instrument]` annotations and structured span
fields:

| Module | Function | Key Span Fields |
|--------|----------|-----------------|
| `app.rs` | `process_files` | `file_count`, `group_count`, `total_duration_ms` |
| `rag.rs` | `build_index` | `file_count`, `chunk_count`, `embed_duration_ms` |
| `rag.rs` | `retrieve_context` | `file_name`, `result_count` |
| `llm/mod.rs` | `process_file` | `file_name`, `file_type`, `pipeline_mode` |
| `llm/mod.rs` | `process_group` | `group_name`, `member_count` |
| `llm/mod.rs` | `extract` | `file_name`, `token_count` |
| `llm/mod.rs` | `generate` | `file_name`, `context_len`, `cache_hit` |
| `llm/mod.rs` | `review` | `file_name` |
| `llm/mod.rs` | `stream_chat_with_progress` | `model`, `token_count`, `duration_ms` |
| `llm/mod.rs` | `run_ollama_chat` / `run_openai_chat` | `model`, `attempt`, `status` |
| `llm/mod.rs` | `describe_image_cloud` / `describe_image_ollama` | `model`, `image_size_bytes`, `token_count` |
| `parser/*.rs` | `parse_word` / `parse_excel` / `parse_visio` | `file_name`, `page_count` or `sheet_count` |
| `cache.rs` | Cache read/write operations | `key`, `hit` |

### 3.5  Example Instrumented Function

```rust
#[tracing::instrument(
    name = "llm.generate",
    skip(self, summary, context_summary, glossary, status_tx, cancel_token),
    fields(file_name, context_len = context_summary.len(), cache_hit)
)]
async fn generate(
    &self,
    file_name: &str,
    summary: &str,
    context_summary: &str,
    glossary: &str,
    status_tx: &std::sync::mpsc::Sender<String>,
    cancel_token: &CancellationToken,
) -> Result<String> {
    // Check cache
    if let Some(cached) = self.check_cache(file_name, summary, context_summary) {
        tracing::Span::current().record("cache_hit", true);
        info!("Cache hit for generation");
        return Ok(cached);
    }
    tracing::Span::current().record("cache_hit", false);

    // ... existing generation logic ...
}
```

### 3.6  UI Integration

Add a toggle in the Settings panel:

- **"Verbose logging"** checkbox → sets `RUST_LOG=debug,rig=trace` at runtime
- **"Export traces (OTEL)"** checkbox → only visible when compiled with `otel`
  feature; configures OTLP endpoint URL

Trace summaries (per-file timing, cache hits) can also be surfaced in the
existing status panel alongside the current progress messages.

---

## 4  Implementation Phases

### Phase 1 — Observability Foundation (Low Risk)

**Goal**: Add structured tracing throughout the codebase without changing any
logic.

**Changes**:
1. Update `tracing-subscriber` features in Cargo.toml to include `"json"`
2. Add `init_tracing()` to `main.rs` with `EnvFilter`
3. Add `#[instrument]` to all functions listed in §3.4
4. Add `info!` / `debug!` / `warn!` at key decision points (cache hit/miss,
   retry, fallback)
5. Replace existing `println!`-style debugging with `tracing` macros

**Validation**: Run with `RUST_LOG=debug cargo run` and verify structured log
output covers the full pipeline.

### Phase 2 — RAG Index Build (Medium Risk)

**Goal**: Replace the dead `rag.rs` with a working rig-core + MongoDB-backed
implementation and build the index during processing.

**Changes**:
1. Delete all existing code in `rag.rs`
2. Implement `chunk` submodule (migrate chunking logic from old `rag.rs`)
3. Implement `build_index()` with rig-core `EmbeddingsBuilder` +
   `MongoDbVectorIndex` (via `rig-mongodb`)
4. Add `chunk_all_files()` to `ProjectContext` in `context.rs`
5. In `app.rs` Phase 1.3, connect to Docker MongoDB, call `rag::build_index()`
   with cancellation support
6. Add Ollama embedding model pull check (ensure `nomic-embed-text` is
   available; if not, log warning and skip RAG)

**Validation**: After Phase 1.3, log the index size (chunk count, embedding
dimension). Verify embeddings are persisted in MongoDB. Verify no runtime
hangs — should be resolved since we use rig-core's HTTP-based embedding
instead of the old TF-IDF approach.

### Phase 3 — RAG Retrieval & Injection (Medium Risk)

**Goal**: Use the RAG index during per-file/group processing to provide
semantically relevant cross-file context.

**Changes**:
1. Implement `retrieve_context()` in `rag.rs`
2. In the per-file task body (app.rs), call `retrieve_context()` before
   `process_file()`
3. Pass retrieved context as `rag_context: Some(context)` instead of `None`
4. In `process_file` / `process_group`, prefer `rag_context` over
   `context.build_summary()` when available
5. For groups: exclude all group member files from retrieval results
6. Apply `MAX_CONTEXT_CHARS` budget to retrieved context (truncate if needed)

**Validation**: Compare generated Gherkin output for a multi-file project with
and without RAG. The RAG version should reference concepts from related files
more accurately.

### Phase 4 — Embedding Model Configuration (Low Risk)

**Goal**: Let users choose their embedding model and provider.

**Changes**:
1. Add UI dropdown: "Embedding Model" with options:
   - `nomic-embed-text` (Ollama, default)
   - `text-embedding-3-small` (OpenAI)
   - `mxbai-embed-large` (Ollama, higher quality)
   - `AllMiniLML6V2Q` (FastEmbed, local CPU — no external service)
   - Auto (try Ollama → OpenAI → FastEmbed, in order)
   - None (disable RAG, use excerpt fallback)
2. Store selection in app state alongside existing provider config
3. Validate model availability at startup (Ollama: check `/api/tags`; OpenAI:
   presence of API key; FastEmbed: always available)

**Validation**: Switch between providers and verify index builds correctly with
each.

### Phase 5 — OpenTelemetry Export (Low Risk, Optional)

**Goal**: Enable production-grade trace export for users who need it.

**Changes**:
1. Add `otel` feature flag to Cargo.toml (§3.2)
2. Implement conditional OTEL layer in `init_tracing()` (§3.3)
3. Add OTEL endpoint configuration in Settings UI
4. Add OTEL collector config template (`otel/config.yaml`) for Langfuse/Jaeger
5. Document setup in README

**Validation**: Run with OTEL collector, verify traces appear in backend
(Jaeger UI or Langfuse dashboard).

### Phase 6 — Persistent Cross-Session Memory (Medium Risk)

**Goal**: Persist LLM-generated summaries (factoids) across runs so that
knowledge accumulates over time. File-chunk embeddings are already persisted in
MongoDB from Phase 2. This phase adds a separate `memories` collection for
higher-level factoids extracted from generated output.

Inspired by the Rig + MongoDB "AI Memories" pattern
(ref: `rig-mongodb` crate, `MongoDbVectorIndex`).

#### 6.1  Motivation

File-chunk embeddings (Phase 2) capture raw document content. But after each
processing run the LLM produces Gherkin scenarios that contain distilled domain
knowledge — business rules, entity relationships, integration boundaries. By
extracting factoids from these outputs and storing them as a second set of
embeddings in MongoDB, subsequent runs gain access to cross-run insights that go
beyond raw file text.

- **Cross-run knowledge**: Summaries and factoids from previous LLM outputs
  become retrievable context for future runs.
- **Project memory**: Over successive runs, the system builds a richer
  understanding of the project's domain language, architecture patterns, and
  business rules.
- **Incremental by default**: File-chunk upserts (Phase 2) already skip
  unchanged content via `_id`-based `replace_one`. Factoids accumulate
  additively.

#### 6.2  Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                   MongoDB (Docker)                               │
│                                                                  │
│  ┌─────────────────────────────┐  ┌────────────────────────────┐ │
│  │ Collection: chunks          │  │ Collection: memories       │ │
│  │                             │  │                            │ │
│  │ • File chunk embeddings     │  │ • Summarized factoids from │ │
│  │ • Upserted each run         │  │   previous LLM outputs     │ │
│  │ • _id = file:chunk_idx      │  │ • run_id + timestamp       │ │
│  │ • Atlas Search index:       │  │ • Atlas Search index:      │ │
│  │   "vector_index"            │  │   "memory_vector_index"    │ │
│  └─────────────┬───────────────┘  └──────────────┬─────────────┘ │
│                │    Combined retrieval            │               │
│                └──────────┬───────────────────────┘               │
│                           ▼                                      │
│                  Merged RAG context                               │
│                  (chunks + historical factoids)                   │
└──────────────────────────────────────────────────────────────────┘
```

#### 6.3  Memory Struct

Adapting the `#[derive(Embed)]` pattern from `rig-core`:

```rust
use rig::Embed;
use serde::{Deserialize, Serialize};

/// A persistent memory entry — a summarized factoid from a previous LLM run.
#[derive(Embed, Clone, Serialize, Deserialize, Debug)]
pub struct ProjectMemory {
    pub id: String,
    /// Identifies the processing run (e.g. nanoid or timestamp).
    pub run_id: String,
    /// Source file path that produced this factoid, or "summary".
    pub source: String,
    /// The embeddable text.
    #[embed]
    pub memory: String,
    /// Unix timestamp when this memory was created.
    pub created_at: i64,
}
```

#### 6.4  Run Summarization (Factoid Extraction)

After each processing run completes, summarize the generated Gherkin output into
a set of factoids (key domain concepts, business rules, entity relationships)
and store them as persistent memories in the `memories` collection. This mirrors
the article's `summarize_chunks` pattern:

```rust
use nanoid::nanoid;
use chrono::Utc;
use mongodb::bson::doc;
use rig::embeddings::EmbeddingsBuilder;

pub async fn extract_and_store_factoids<M>(
    llm_client: &impl rig::completion::CompletionModel,
    embedding_model: &M,
    memories_collection: &Collection<bson::Document>,
    generated_outputs: &[(String, String)], // (file_name, gherkin_output)
) -> Result<()>
where
    M: rig::embeddings::EmbeddingModel + Clone,
{
    let combined = generated_outputs
        .iter()
        .map(|(name, text)| format!("## {name}\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    let extractor = llm_client
        .extractor::<Vec<String>>()
        .preamble(
            "Extract a list of key factoids from the following generated \
             Gherkin test scenarios. Focus on: domain entities, business rules, \
             system boundaries, integration points, and recurring patterns. \
             Return as a JSON array of concise strings."
        )
        .build();

    let factoids = extractor.extract(&combined).await?;
    let run_id = nanoid!(6);
    let now = Utc::now().timestamp();

    let memories: Vec<ProjectMemory> = factoids
        .into_iter()
        .map(|text| ProjectMemory {
            id: nanoid!(10),
            run_id: run_id.clone(),
            source: "summary".to_string(),
            memory: text,
            created_at: now,
        })
        .collect();

    let embeddings = EmbeddingsBuilder::new(embedding_model.clone())
        .documents(memories)?
        .build()
        .await?;

    let mongo_docs = embeddings
        .iter()
        .map(|(mem, embedding)| {
            doc! {
                "_id": &mem.id,
                "run_id": &mem.run_id,
                "source": &mem.source,
                "memory": &mem.memory,
                "created_at": mem.created_at,
                "embedding": embedding.first().vec.clone(),
            }
        })
        .collect::<Vec<_>>();

    memories_collection.insert_many(mongo_docs).await?;
    info!("Saved {} factoid memories to MongoDB", embeddings.len());

    Ok(())
}
```

#### 6.5  Retrieving Memories

During retrieval (Phase 3 augmentation), query both the `chunks` collection
and the `memories` collection, then merge results:

```rust
let memories_index = MongoDbVectorIndex::new(
    memories_collection.clone(),
    embedding_model.clone(),
    "memory_vector_index",
    SearchParams::new(),
).await?;

let chunk_results = rag_index.top_n::<String>(query, 4).await?;
let memory_results = memories_index.top_n::<ProjectMemory>(query, 3).await?;

// Merge: file chunks first, then historical factoids
let rag_context = format!(
    "{chunks}\n\n--- Historical memories ---\n{memories}",
    chunks = format_chunk_results(&chunk_results),
    memories = memory_results
        .into_iter()
        .map(|(score, _, mem)| format!("• (relevance: {score:.2}) {}", mem.memory))
        .collect::<Vec<_>>()
        .join("\n"),
);
```

#### 6.6  Incremental Indexing

File-chunk embeddings are already incremental via Phase 2's `replace_one` with
`upsert(true)` keyed on `_id = "file:chunk_idx"`. On each run:

- **Unchanged files**: Same `_id` → upsert is a no-op (content unchanged).
- **Changed files**: Same `_id` → upsert replaces with new embedding.
- **Deleted files**: Prune orphaned chunks via a cleanup pass:
  ```rust
  // After indexing, delete chunks whose source file no longer exists
  let active_prefixes: Vec<String> = project_context
      .file_contents.keys().cloned().collect();
  collection.delete_many(
      doc! { "_id": { "$not": { "$regex": active_prefixes.join("|") } } }
  ).await?;
  ```
- **Factoid memories**: Accumulate additively. Optional TTL-based pruning
  (e.g. delete memories older than 30 days).

#### 6.7  MongoDB Vector Search Index Setup

The `memories` collection needs an Atlas Search vector index (created
automatically via the Docker init script or manually):

```json
{
  "fields": [
    {
      "numDimensions": 768,
      "path": "embedding",
      "similarity": "cosine",
      "type": "vector"
    }
  ]
}
```

(Use `numDimensions: 1536` when using OpenAI `text-embedding-3-small`.)

#### 6.8  Changes

1. Implement `ProjectMemory` struct with `Embed` derive
2. Implement `extract_and_store_factoids()` for post-run summarization
3. Add `connect_to_memories()` helper in `rag.rs`
4. Augment retrieval to query both `chunks` and `memories` collections
5. Add content-hash-based incremental indexing (orphan cleanup)
6. Add `nanoid` and `chrono` dependencies
7. Add `memory_vector_index` to MongoDB Docker init script

**Validation**: Process a project twice — second run should include factoids
from the first run in cross-file context. Verify memories accumulate in MongoDB.

### Phase 7 — `dynamic_context()` Agent Integration (Future Enhancement)

**Goal**: Use rig-core's native `dynamic_context()` to automatically inject RAG
results per-agent-call, removing manual retrieval logic.

**Changes**:
1. Restructure agent construction to accept `VectorStoreIndex`
2. Use `.dynamic_context(4, rag_index)` on agent builder
3. Remove manual `retrieve_context()` calls
4. Profile token usage to calibrate `n` (number of chunks retrieved)

**Validation**: Verify same or better output quality with simpler code path.

---

## 5  Cancellation & Structured Concurrency Integration

All new async operations must follow the patterns established in
`docs/CANCELLATION_AND_STRUCTURED_CONCURRENCY.md`:

| Operation | Cancellation Strategy |
|-----------|-----------------------|
| Embedding API call (`EmbeddingsBuilder::build()`) | `tokio::select!` with `cancel_token.cancelled()` |
| Vector search (`index.top_n()`) | Async MongoDB round-trip; wrap in `tokio::select!` with cancel token |
| MongoDB upsert (`replace_one` loop) | Check cancel token between batches |
| Ollama model availability check | Timeout + `select!` with cancel token |
| OTEL span export (background) | Graceful shutdown via `opentelemetry::global::shutdown_tracer_provider()` |

Child tokens: The RAG indexing phase runs on the main processing token. Per-file
retrieval runs on per-task child tokens (already created in `process_files()`).

---

## 6  Error Handling & Fallback

| Failure Mode | Behaviour |
|-------------|-----------|
| MongoDB container not running | `warn!` log, skip RAG, use `build_summary()` fallback |
| Embedding model not available (Ollama) | Fall back to FastEmbed (local CPU); if FastEmbed also fails, use `build_summary()` fallback |
| Embedding API returns error (OpenAI) | Fall back to FastEmbed (local CPU); if FastEmbed also fails, use `build_summary()` fallback |
| Embedding API timeout | Respect cancel token → bail if cancelled, otherwise retry once then fall back to FastEmbed |
| Zero chunks (empty project) | Skip RAG, proceed without cross-file context |
| MongoDB vector index missing | `warn!` log with setup instructions, fall back to excerpt context |
| OTEL collector unreachable | Non-blocking — `tracing-opentelemetry` drops spans silently |

---

## 7  Performance Considerations

| Concern | Mitigation |
|---------|-----------|
| Embedding latency (Ollama) | `nomic-embed-text` embeds ~1000 tokens/sec on GPU; typical project (20 files × 5 chunks) = ~100 chunks ≈ seconds |
| Embedding latency (OpenAI) | Batch API, ~50ms per batch of 100 chunks |
| Embedding latency (FastEmbed) | CPU-only, ~60 ms/chunk with `AllMiniLML6V2Q` (quantized). ~100 chunks ≈ 6 sec on a modern CPU. Slower than GPU but always available |
| MongoDB storage | 100 chunks × 768 dims × 4 bytes ≈ 300 KB per project — negligible for local Docker volume |
| Re-indexing on re-run | Upsert-based: unchanged chunks are overwritten with identical data (fast); only changed chunks need new embeddings |
| Search latency | MongoDB Atlas Search vector index — sub-100ms for <1000 vectors |
| Docker overhead | MongoDB container uses ~200 MB RAM at idle; acceptable alongside Ollama instances |
| FastEmbed model download | ONNX model downloaded on first use (~23 MB for AllMiniLML6V2Q) and cached locally. Subsequent runs use cache |

---

## 8  Dependency Summary

| Crate | Version | Purpose | Required? |
|-------|---------|---------|-----------|
| `rig-core` | 0.32.0 | Embedding models, `VectorStoreIndex` | ✅ Already present |
| `rig-mongodb` | latest | MongoDB vector store (`MongoDbVectorIndex`) for chunk + memory persistence | ✅ Required |
| `rig-fastembed` | latest | Local CPU-only embeddings via FastEmbed ONNX models (`AllMiniLML6V2Q` etc.) — zero external service fallback | ✅ Required |
| `mongodb` | latest | MongoDB Rust driver | ✅ Required |
| `nanoid` | 0.4 | Short unique IDs for memory entries / run IDs | ✅ Required |
| `chrono` | 0.4 | Timestamps for memory entries | ✅ Required |
| `tracing` | 0.1 | Structured logging spans | ✅ Already present |
| `tracing-subscriber` | 0.3 | Log formatting + filtering | ✅ Present, add `"json"` feature |
| `tokio-util` | 0.7 | CancellationToken, TaskTracker | ✅ Already present |
| `tracing-opentelemetry` | 0.30 | OTEL trace bridge | Optional (`otel` feature) |
| `opentelemetry` | 0.31 | OTEL API | Optional (`otel` feature) |
| `opentelemetry_sdk` | 0.31 | OTEL SDK runtime | Optional (`otel` feature) |
| `opentelemetry-otlp` | 0.31 | OTLP exporter | Optional (`otel` feature) |

---

## 9  File Change Map

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `rig-mongodb`, `rig-fastembed`, `mongodb`, `nanoid`, `chrono`; add `"json"` to `tracing-subscriber` features; add optional OTEL deps; add `[features]` section |
| `docker-compose.yml` | Add `mongo` service (MongoDB 7 with vector search) |
| `src/main.rs` | Add `init_tracing()` call |
| `src/rag.rs` | **Full rewrite** — rig-core embeddings (Ollama/OpenAI/FastEmbed), `MongoDbVectorIndex`, `build_index()`, `retrieve_context()`, `connect_to_mongodb()`, `connect_to_memories()`, FastEmbed fallback logic |
| `src/context.rs` | Add `chunk_all_files()` method |
| `src/app.rs` | Phase 1.3: connect to MongoDB, call `build_index()`; per-file: call `retrieve_context()`; pass `Some(rag_ctx)` to LLM pipeline; post-run: call `extract_and_store_factoids()` |
| `src/llm/mod.rs` | Add `#[instrument]` to all pipeline functions; no logic changes |
| `src/llm/prefix_cache.rs` | Add `#[instrument]` |
| `src/parser/*.rs` | Add `#[instrument]` |
| `src/cache.rs` | Add `#[instrument]` |
| `src/memory.rs` | **New** — `ProjectMemory` struct, `extract_and_store_factoids()`, memory retrieval helpers |
| `docs/RAG_AND_OBSERVABILITY_PLAN.md` | This document |

---

## 10  Success Criteria

- [ ] **RAG index builds** without panics or hangs for projects of 1–50 files
- [ ] **Cross-file context** references semantically relevant content (not just
      first-400-char excerpts)
- [ ] **Cancellation** interrupts RAG indexing within 1 second of cancel click
- [ ] **Fallback** works when embedding model is unavailable
- [ ] **Structured traces** visible with `RUST_LOG=debug` showing full pipeline
      span tree
- [ ] **OTEL export** (when enabled) produces valid traces in Jaeger/Langfuse
- [ ] **No regression** in processing speed for small projects (RAG overhead
      < 5 seconds for 20 files)
- [ ] **Persistent memory** survives across runs — second run retrieves
      factoids from first run
- [ ] **Incremental indexing** skips unchanged files on re-run (measured by
      embedding API call count)
- [ ] **Existing tests** continue to pass
