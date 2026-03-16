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
│             │    chunk → embed → insert into   │        │
│             │    InMemoryVectorStore            │        │
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
```

### 2.2  Embedding Strategy

**Two modes**, matching existing provider support:

| Provider | Embedding Model | Dim | Notes |
|----------|----------------|-----|-------|
| Ollama (local) | `nomic-embed-text` (or configurable) | 768 | Free, no API key, fast on GPU. Pulled automatically if missing. |
| OpenAI (cloud) | `text-embedding-3-small` | 1536 | Requires API key; low cost ($0.02/1M tokens). |

The embedding model **must match** between indexing and querying — rig-core
enforces this via its type system (`EmbeddingModel` passed to both
`EmbeddingsBuilder` and `vector_store.index(model)`).

**Fallback**: If no embedding model is available (e.g. Ollama is down and no
OpenAI key), fall back to the existing excerpt-based `build_summary()` approach
so the pipeline still runs. Log a warning via `tracing::warn!`.

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
use rig::embeddings::EmbeddingsBuilder;
use rig::vector_store::in_memory_store::InMemoryVectorStore;

// Inside process_files(), after Phase 1.25:

let embedding_model = match &orchestrator.provider {
    Provider::Ollama { .. } => {
        ollama_client.embedding_model("nomic-embed-text")
    }
    Provider::OpenAI { .. } => {
        openai_client.embedding_model(TEXT_EMBEDDING_3_SMALL)
    }
};

let mut vector_store = InMemoryVectorStore::default();

// Chunk all files
let chunks: Vec<(String, String)> = project_context
    .file_contents
    .values()
    .flat_map(|fc| {
        chunk_text(&fc.raw_text, &fc.path, &fc.file_type)
            .into_iter()
            .map(|c| (c.document_id(), c.text.clone()))
    })
    .collect();

// Embed in batches (rig-core handles batching internally)
let embeddings = EmbeddingsBuilder::new(embedding_model.clone())
    .documents(chunks)?
    .build()
    .await?;

vector_store.add_documents(embeddings);

// Create searchable index — Arc-wrapped for sharing across tasks
let rag_index = Arc::new(vector_store.index(embedding_model));
```

**Cancellation**: The `EmbeddingsBuilder::build()` call is a single await point.
Wrap in `tokio::select!` with the cancel token:

```rust
tokio::select! {
    result = EmbeddingsBuilder::new(model.clone())
        .documents(chunks)?
        .build() => {
        let embeddings = result?;
        vector_store.add_documents(embeddings);
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

**Cancellation**: The `top_n` call is CPU-only for `InMemoryVectorStore` (cosine
similarity), so it completes near-instantly. No explicit cancellation needed
here, but wrap in `select!` for consistency.

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
//! RAG pipeline using rig-core's embedding + vector store APIs.

use anyhow::Result;
use rig::embeddings::EmbeddingsBuilder;
use rig::vector_store::in_memory_store::InMemoryVectorStore;
use rig::vector_store::{VectorSearchRequest, VectorStoreIndex};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

// Re-export chunk types
pub use chunk::{TextChunk, chunk_text, build_query_text};

mod chunk {
    // Chunking logic (migrated from current rag.rs, cleaned up)
}

/// Build the RAG index from all project files.
///
/// Returns the searchable index, or `None` if embedding fails
/// (in which case the caller should fall back to excerpt context).
#[instrument(skip_all, fields(file_count, chunk_count))]
pub async fn build_index<M>(
    model: M,
    files: &crate::context::ProjectContext,
    cancel_token: &CancellationToken,
) -> Result<Option<impl VectorStoreIndex>>
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

    let mut store = InMemoryVectorStore::default();

    tokio::select! {
        result = EmbeddingsBuilder::new(model.clone())
            .documents(documents)?
            .build() => {
            store.add_documents(result?);
            info!("RAG index built");
            Ok(Some(store.index(model)))
        }
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled during RAG indexing");
        }
    }
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
    let request = VectorSearchRequest::builder()
        .query(query_text)
        .samples(top_n + 2) // over-fetch to compensate for exclusion
        .build()?;

    let results = index.top_n::<String>(request).await?;

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

**Goal**: Replace the dead `rag.rs` with a working rig-core-backed
implementation and build the index during processing.

**Changes**:
1. Delete all existing code in `rag.rs`
2. Implement `chunk` submodule (migrate chunking logic from old `rag.rs`)
3. Implement `build_index()` with rig-core `EmbeddingsBuilder` +
   `InMemoryVectorStore`
4. Add `chunk_all_files()` to `ProjectContext` in `context.rs`
5. In `app.rs` Phase 1.3, call `rag::build_index()` with cancellation support
6. Add Ollama embedding model pull check (ensure `nomic-embed-text` is
   available; if not, log warning and skip RAG)

**Validation**: After Phase 1.3, log the index size (chunk count, embedding
dimension). Verify no runtime hangs (the old issue) — should be resolved since
we use rig-core's HTTP-based embedding instead of the old TF-IDF approach.

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
   - None (disable RAG, use excerpt fallback)
2. Store selection in app state alongside existing provider config
3. Validate model availability at startup (Ollama: check `/api/tags`; OpenAI:
   presence of API key)

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

### Phase 6 — `dynamic_context()` Agent Integration (Future Enhancement)

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
| Vector search (`index.top_n()`) | Near-instant for in-memory store; wrap in `select!` for consistency |
| Ollama model availability check | Timeout + `select!` with cancel token |
| OTEL span export (background) | Graceful shutdown via `opentelemetry::global::shutdown_tracer_provider()` |

Child tokens: The RAG indexing phase runs on the main processing token. Per-file
retrieval runs on per-task child tokens (already created in `process_files()`).

---

## 6  Error Handling & Fallback

| Failure Mode | Behaviour |
|-------------|-----------|
| Embedding model not available | `warn!` log, skip RAG, use `build_summary()` fallback |
| Embedding API returns error | `warn!` log, skip RAG for that run, use fallback |
| Embedding API timeout | Respect cancel token → bail if cancelled, otherwise retry once then fallback |
| Zero chunks (empty project) | Skip RAG, proceed without cross-file context |
| OTEL collector unreachable | Non-blocking — `tracing-opentelemetry` drops spans silently |

---

## 7  Performance Considerations

| Concern | Mitigation |
|---------|-----------|
| Embedding latency (Ollama) | `nomic-embed-text` embeds ~1000 tokens/sec on GPU; typical project (20 files × 5 chunks) = ~100 chunks ≈ seconds |
| Embedding latency (OpenAI) | Batch API, ~50ms per batch of 100 chunks |
| Memory for vector store | 100 chunks × 768 dims × 4 bytes = ~300 KB — negligible |
| Re-indexing on re-run | Index is rebuilt each run (files may have changed); cached Gherkin output is still LLM-cache-keyed by context hash |
| Search latency | Brute-force cosine on <1000 vectors is sub-millisecond |

---

## 8  Dependency Summary

| Crate | Version | Purpose | Required? |
|-------|---------|---------|-----------|
| `rig-core` | 0.32.0 | Embedding models, `InMemoryVectorStore`, `VectorStoreIndex` | ✅ Already present |
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
| `Cargo.toml` | Add `"json"` to `tracing-subscriber` features; add optional OTEL deps; add `[features]` section |
| `src/main.rs` | Add `init_tracing()` call |
| `src/rag.rs` | **Full rewrite** — rig-core embeddings, `InMemoryVectorStore`, `build_index()`, `retrieve_context()` |
| `src/context.rs` | Add `chunk_all_files()` method |
| `src/app.rs` | Phase 1.3: call `build_index()`; per-file: call `retrieve_context()`; pass `Some(rag_ctx)` to LLM pipeline |
| `src/llm/mod.rs` | Add `#[instrument]` to all pipeline functions; no logic changes |
| `src/llm/prefix_cache.rs` | Add `#[instrument]` |
| `src/parser/*.rs` | Add `#[instrument]` |
| `src/cache.rs` | Add `#[instrument]` |
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
- [ ] **Existing tests** continue to pass
