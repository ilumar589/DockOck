# Architecture

This document describes the design decisions and component boundaries of DockOck.

---

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        DockOck Process                            │
│                                                                   │
│  ┌──────────────┐   mpsc channel   ┌──────────────────────────┐  │
│  │  egui / UI   │◄─────────────────│   Async Processing       │  │
│  │  (main thd)  │                  │   (tokio multi-thread)   │  │
│  └──────────────┘                  └──────────┬───────────────┘  │
│         │                                     │                   │
│         │ rfd::FileDialog            ┌────────┴────────┐         │
│         │                            │  File Parsers   │         │
│         ▼                            │  word / excel   │         │
│  ┌──────────────┐                    │  / visio        │         │
│  │ ProjectContext│                    └────────┬────────┘         │
│  │ (Arc<Mutex>) │                             │                   │
│  └──────────────┘                    ┌────────▼────────┐         │
│                                      │   LLM Pipeline  │         │
│  ┌──────────────┐                    │  Extract →      │         │
│  │  DiskCache   │◄──────────────────►│  Generate →     │         │
│  │ (SHA-256)    │                    │  Review         │         │
│  └──────────────┘                    └────────┬────────┘         │
│                                               │                   │
│  ┌──────────────┐                    ┌────────▼────────┐         │
│  │  Session     │                    │  RAG + Memory   │         │
│  │  Persistence │                    │  (MongoDB)      │         │
│  └──────────────┘                    └─────────────────┘         │
└──────────────────────────────────────────────────────────────────┘
         │                    │                    │
    File Dialog          HTTP REST            Docker
         │                    │                    │
         ▼                    ▼                    ▼
  ┌────────────┐   ┌──────────────────┐   ┌──────────────┐
  │  OS native │   │ Ollama (x4)      │   │ MongoDB 7    │
  │  picker    │   │ gen / ext / rev / │   │ (Atlas local)│
  └────────────┘   │ vision           │   │ :27017       │
                   │ :11434-11437     │   └──────────────┘
                   └──────────────────┘
```

---

## Module Breakdown

### `src/main.rs`

Entry point. Responsibilities:
- Load `.env` file for API keys and custom provider settings.
- Initialise structured tracing (`init_tracing()`) with `EnvFilter`, JSON-capable
  fmt layer, and optional OpenTelemetry export (`otel` feature flag).
- Create a `tokio::runtime::Runtime` and keep it alive in a background thread.
- Configure and launch the `eframe` native window.

### `src/app.rs`

The egui application (`DockOckApp`). Responsibilities:
- Own all UI state: file list, results, groups, ratings, model selections,
  embedding choice, backend selection.
- Render four UI regions: top bar (models, pipeline, RAG), left panel (files &
  groups), right panel (Gherkin output with diff/rating), bottom bar (log & progress).
- Spawn the background `process_files()` task via the Tokio handle.
- Poll the `mpsc::Receiver<ProcessingEvent>` every frame and apply incoming events.
- `process_files()`: The main async pipeline that orchestrates parsing, RAG
  indexing, LLM processing, factoid extraction, and OpenSpec export.

### `src/context.rs`

`ProjectContext` – the shared context accumulator. Responsibilities:
- Store extracted `FileContent` for every file processed so far.
- Build a compact text summary (`build_summary()`) for excerpt-based fallback context.
- Extract entities (`extract_entities()`) and build glossary strings.
- Auto-detect file groups by stem matching (`compute_auto_groups()`).
- Chunk all files for RAG indexing (`chunk_all_files()`).

### `src/gherkin.rs`

Gherkin data structures. Responsibilities:
- `GherkinDocument` holds a parsed feature with scenarios and steps.
- `to_feature_string()` renders the struct as valid Gherkin syntax.
- `parse_from_llm_output()` parses the LLM's free-form text into the struct.
- Derives `Serialize`/`Deserialize` for session persistence and diffing.

### `src/parser/`

File parsers. Each parser extracts plain text from its format:

| Module | Format | Approach |
|--------|--------|---------|
| `word.rs` | `.docx` | Unzip → parse `word/document.xml` → collect `<w:t>` elements; structured doc representation |
| `excel.rs` | `.xlsx` / `.xls` / `.ods` | `calamine` library; iterate worksheets + rows |
| `visio.rs` | `.vsdx` | Unzip → parse `visio/pages/pageN.xml` → collect `<Text>` and `<Cell N="Label">` elements |

`mod.rs` dispatches to the correct parser based on file extension. All parsers
are annotated with `#[instrument]` for tracing.

### `src/llm/mod.rs`

LLM integration via `rig-core`. Responsibilities:
- `AgentOrchestrator` — manages up to 4 Ollama clients (generator, extractor,
  reviewer, vision) or a single OpenAI-compatible client for custom providers.
- Three-phase pipeline: Extract → Generate → Review (configurable via `PipelineMode`).
- Chunk-and-merge for oversized documents (`process_file_chunked()`).
- KV-cache prefix priming (`src/llm/prefix_cache.rs`) for reduced latency.
- Multi-turn chat format with separate glossary/context/summary turns.
- Cloud and local vision support for image descriptions.
- Concurrency controlled by `tokio::sync::Semaphore`.
- All 13+ functions annotated with `#[instrument]` for full span tree visibility.

### `src/llm/provider.rs`

Provider abstraction. Responsibilities:
- `ProviderBackend` enum: `Ollama` vs `Custom { config_name }`.
- `CustomProviderConfig` loaded from `custom_providers.json`: base URL, API key,
  model mappings, model limits.
- Helper utilities for model info lookup.

### `src/rag.rs`

RAG (Retrieval-Augmented Generation) pipeline. Responsibilities:
- Chunk document text into overlapping windows (`chunk_text()`).
- Embed chunks via Ollama or FastEmbed (local CPU fallback).
- Store embeddings in MongoDB `chunks` collection with upsert semantics.
- Retrieve top-N relevant chunks from other files (`retrieve_context()`).
- Merged retrieval across chunks + memories (`retrieve_full_context()`).
- Automatic vector search index creation (`ensure_search_indexes()`).
- Orphan chunk cleanup for deleted files.
- `EmbeddingChoice` enum for UI: Auto, Ollama models, FastEmbed, None.

### `src/memory.rs`

Persistent cross-session memory. Responsibilities:
- `ProjectMemory` struct with `#[derive(Embed)]` for rig-core embedding.
- Extract factoids from generated Gherkin via LLM (`extract_and_store_factoids()`).
- Store factoid embeddings in MongoDB `memories` collection.
- Retrieve historical factoids during RAG retrieval (`retrieve_memories()`).
- Heuristic fallback extraction when no LLM is available (FastEmbed-only mode).

### `src/cache.rs`

Content-addressed disk cache. Responsibilities:
- SHA-256 keyed file-system cache under `<output_dir>/.dockock_cache/`.
- Namespaces: `parsed`, `vision`, `llm`, `openspec`, `embedding`.
- Sync and async get/put operations.
- Cache eliminates redundant parsing, vision calls, and LLM calls on re-runs.

### `src/session.rs`

Session persistence. Responsibilities:
- Save/load project state to `<output_dir>/.dockock_session.json`.
- Preserves file list, groups, results, ratings, model selections.
- LCS-based line-level diff (`diff_gherkin()`) for comparing regenerated output.

### `src/openspec.rs`

OpenSpec integration. Responsibilities:
- HTTP client for the containerised OpenSpec service.
- Post Gherkin text, receive change artifacts (proposal, spec, tasks, design).
- Save artifacts to `<output_dir>/openspec/<change_name>/`.

---

## Threading Model

The egui event loop runs on the **main thread**. All async work runs on a
**multi-threaded Tokio runtime** that persists for the process lifetime.

Events (status updates, file results) travel from async tasks to the UI thread
through a `std::sync::mpsc` channel. The UI polls this channel every frame.

```
Main thread                     Tokio Runtime (multi-thread)
──────────────────────────────────────────────────────────────
eframe::run_native()
  └─ App::update() each frame
       └─ poll_events()         ◄── mpsc channel
                                       │
                                process_files()
                                  ├─ Parse files (spawn_blocking)
                                  ├─ Build RAG index (MongoDB)
                                  ├─ Per-file/group LLM tasks
                                  │   (TaskTracker + Semaphore)
                                  ├─ Extract factoid memories
                                  └─ OpenSpec export (optional)
```

### Cancellation & Structured Concurrency

All async work respects `CancellationToken` from `tokio-util`:
- Parent token owned by `DockOckApp`, child tokens per task.
- `tokio::select!` with cancel token wraps every await point.
- `TaskTracker` tracks all spawned tasks for graceful shutdown.

See `docs/CANCELLATION_AND_STRUCTURED_CONCURRENCY.md` for full details.

---

## RAG & Cross-File Context

### Semantic RAG (Primary)

When MongoDB is available and an embedding model is configured:

1. **Index Build**: All parsed files are chunked (1024 chars, 256 overlap) and
   embedded via Ollama or FastEmbed. Embeddings are upserted into MongoDB.
2. **Retrieval**: Before each LLM call, query the vector index for top-4 chunks
   from *other* files, plus top-3 historical factoid memories.
3. **Injection**: Merged context string replaces excerpt-based summaries.

### Excerpt Fallback

When RAG is unavailable (no MongoDB, embedding failure, or user disabled):
- `context.build_summary()` provides first-400-char excerpts per file.
- Fully automatic — no configuration needed.

### Embedding Providers

| Provider | Model | Dimensions | Notes |
|----------|-------|-----------|-------|
| Ollama | `nomic-embed-text` | 768 | GPU-accelerated, default |
| Ollama | `mxbai-embed-large` | 1024 | Higher quality |
| FastEmbed | `AllMiniLML6V2Q` | 384 | Local CPU, zero-dependency fallback |

---

## Observability

- **Structured tracing**: All modules annotated with `#[instrument]`.
- **Log levels**: Configurable via `RUST_LOG` env var (default: `info,dockock=debug`).
- **OpenTelemetry**: Optional OTEL trace export via `--features otel`.
  Traces include LLM calls, file parsing, embedding, retrieval, and cache operations.

---

## Docker Services

| Service | Port | Purpose |
|---------|------|---------|
| `ollama-generator` | 11434 | Primary LLM (qwen2.5-coder:32b) |
| `ollama-extractor` | 11435 | Document extraction + embedding models |
| `ollama-reviewer` | 11436 | Gherkin review |
| `ollama-vision` | 11437 | Image/diagram description (moondream) |
| `mongo` | 27017 | Vector store for RAG chunks & memories |
| `openspec` | 11438 | OpenSpec artifact generator |

---

## Adding Support for New File Types

1. Create `src/parser/<format>.rs`.
2. Implement a `pub fn parse(path: &Path) -> Result<ParseResult>`.
3. Add `#[instrument]` annotation.
4. Register the new extension in `src/parser/mod.rs` inside `parse_file()`.
5. Add any new crate dependencies to `Cargo.toml`.
