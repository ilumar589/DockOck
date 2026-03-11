# Custom Provider Integration Plan

> **Goal**: Allow DockOck to run against the AIArk (ByteDance) OpenAI-compatible
> cloud API for text generation, while keeping **vision local** (Ollama moondream)
> and replacing the in-memory RAG store with **SurrealDB**.

---

## Provider & Model Inventory

**Base URL**: `https://ark.ap-southeast.bytepluses.com/api/v3`  
**Auth**: Bearer token via `AIARK_API_KEY` in `.env`

### Available Models

| Model ID | Display Name | Context | Max Output |
|---|---|---|---|
| `deepseek-v3-2-251201` | DeepSeek V3.2 | 120k | 32,768 |
| `kimi-k2-250905` | Kimi K2 | 256k | 32,768 |
| `kimi-k2-thinking-251104` | Kimi K2 Thinking | 256k | 32,000 |
| `seed-1-8-251228` | Seed 1.8 | 224k | 64,000 |
| `seed-1-6-250915` | Seed 1.6 | 224k | 32,000 |
| `seed-1-6-flash-250715` | Seed 1.6 Flash | 256k | 16,000 |
| `seed-2-0-mini-260215` | Seed 2.0 Mini | 262k | 131,072 |
| `seed-2-0-lite-260228` | Seed 2.0 Lite | 262k | 131,072 |
| `gpt-oss-120b-250805` | GPT OSS 120B | 128k | 64,000 |
| `glm-4-7-251222` | GLM 4.7 | 220k | 131,072 |

### Recommended Role Mapping

| Pipeline Role | AIArk Model | Rationale |
|---|---|---|
| **Generator** | `deepseek-v3-2-251201` | Best code/structured output model |
| **Extractor** | `seed-1-6-flash-250715` | Fast, cheap; extraction is high-volume |
| **Reviewer** | `kimi-k2-thinking-251104` | Reasoning model — ideal for critique |
| **Vision** | `moondream` *(local Ollama)* | No cloud vision model available |
| **Embedding** | `nomic-embed-text` *(local Ollama)* | No cloud embedding model; SurrealDB stores locally |

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│  DockOck App                                         │
│                                                      │
│  ┌────────────────────────────────────────────────┐  │
│  │ AgentOrchestrator                              │  │
│  │                                                │  │
│  │  ┌───────────┐   ┌─────────┐   ┌───────────┐  │  │
│  │  │ Generator  │   │Extractor│   │ Reviewer   │  │  │
│  │  │(OpenAI clnt)  │(OpenAI) │   │(OpenAI)    │  │  │
│  │  └─────┬──────┘   └────┬────┘   └─────┬──────┘  │  │
│  │        │               │               │         │  │
│  │        ▼               ▼               ▼         │  │
│  │  ┌─────────────────────────────────────────┐     │  │
│  │  │  AIArk Cloud API (OpenAI-compatible)    │     │  │
│  │  │  https://ark.ap-southeast.bytepluses.   │     │  │
│  │  │  com/api/v3                             │     │  │
│  │  └─────────────────────────────────────────┘     │  │
│  │                                                │  │
│  │  ┌───────────┐   ┌──────────────────────────┐  │  │
│  │  │  Vision    │   │  RAG Engine              │  │  │
│  │  │ (Ollama)   │   │  Embeddings → Ollama     │  │  │
│  │  │ localhost  │   │  Storage   → SurrealDB   │  │  │
│  │  │ :11437     │   │  (embedded, in-process)  │  │  │
│  │  └───────────┘   └──────────────────────────┘  │  │
│  └────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────┘
```

---

## Dependency Changes

```toml
# New
dotenv = "0.15"                 # .env file loading

# SurrealDB — use 2.5.x to match rig-surrealdb 0.2.1 compatibility
rig-surrealdb = "0.2.1"        # rig vector store trait impl
surrealdb = { version = "2.5", features = ["kv-rocksdb", "kv-mem"] }

# Note: rig-surrealdb 0.2.1 depends on surrealdb 2.5.0.
# SurrealDB 3.0.3 exists but rig-surrealdb hasn't updated yet.
# We use 2.5.x with embedded mode (no separate server process).
# kv-rocksdb → persistent storage in <output_dir>/.dockock_surreal/
# kv-mem    → in-memory fallback when no output dir is set
```

### SurrealDB Version Note

The user linked SurrealDB v3.0.3, but `rig-surrealdb 0.2.1` (the official rig
integration) pins `surrealdb = "2.5.0"`. Two options:

- **Option A (recommended)**: Use `surrealdb 2.5.x` via `rig-surrealdb` for
  seamless rig trait integration. The embedded mode, HNSW vector indexing, and
  SurrealQL all work identically.
- **Option B**: Use `surrealdb 3.0.3` directly, skip `rig-surrealdb`, and
  implement the vector store trait ourselves. More work for no functional gain
  since we're using embedded mode anyway.

---

## Implementation Stages

### Stage 1 — Provider Abstraction

**Files**: `src/llm/mod.rs`, `src/llm/provider.rs` (new), `Cargo.toml`, `src/main.rs`

**Goal**: Introduce a `ProviderBackend` enum that lets the orchestrator create
either Ollama or OpenAI-compatible clients, without changing the pipeline logic.

1. **Add `dotenv` to Cargo.toml** and call `dotenv::dotenv().ok()` in `main.rs`

2. **Create `src/llm/provider.rs`** with:
   ```rust
   pub enum ProviderBackend {
       Ollama,
       Custom {
           name: String,
           base_url: String,
           api_key: String,
       },
   }

   pub struct ModelLimits {
       pub context_tokens: usize,
       pub max_output_tokens: usize,
   }

   pub struct CustomProviderConfig {
       pub name: String,
       pub base_url: String,
       pub models: HashMap<String, ModelLimits>,
   }
   ```

3. **Parse `custom_providers.json`** at startup → `Vec<CustomProviderConfig>`

4. **Load `.env`** → read `AIARK_API_KEY`, `AIARK_BASE_URL`

5. **Wrap rig clients in an enum**:
   ```rust
   pub enum LlmClient {
       Ollama(rig::providers::ollama::Client),
       OpenAi(rig::providers::openai::Client),
   }
   ```
   Both providers produce agents that implement the same rig `Prompt` /
   `Chat` / `StreamPrompt` traits, so the pipeline code stays unchanged.

6. **Update `context_window_for_model()` and `input_budget_for_model()`**:
   When backend is `Custom`, look up limits from `CustomProviderConfig.models`
   instead of pattern-matching model names.

**Deliverable**: `ProviderBackend` enum exists, `.env` is loaded, JSON is
parsed. No pipeline changes yet — this stage only adds the abstraction layer.

---

### Stage 2 — Dual Client Construction

**Files**: `src/llm/mod.rs`

**Goal**: `AgentOrchestrator::new()` accepts a `ProviderBackend` and creates
the appropriate clients for each role.

1. **Modify `AgentOrchestrator` struct**:
   - Replace `generator_client: ollama::Client` etc. with `LlmClient`
   - Add `backend: ProviderBackend` field
   - Keep `vision_endpoint_url: String` (always local Ollama)

2. **Constructor branching**:
   - **Ollama backend**: Existing logic (TCP probe, per-endpoint clients)
   - **Custom backend**: Single `openai::Client` with `base_url` + `api_key`
     shared across generator/extractor/reviewer roles. No TCP probing — all
     roles hit the same cloud URL. Vision stays local Ollama.

3. **Agent building**:
   - **Ollama**: `client.agent(model).preamble(p).additional_params({"num_ctx": N}).build()`
   - **Custom (OpenAI API)**: `client.agent(model).preamble(p).build()`
     (no `num_ctx` — the provider manages context windows; set `max_tokens`
     for output cap if needed via `additional_params({"max_tokens": N})`)

4. **Conditional features**:
   - **PrefixCache**: Only initialise when backend is `Ollama`; wrap in
     `Option<tokio::sync::Mutex<PrefixCache>>`. Skip prefix-cache path in
     `generate()` / `generate_group()` when `None`.
   - **Warm-up**: Skip when backend is `Custom` (hosted APIs have no cold start)

**Deliverable**: The orchestrator can be constructed with either backend.
Pipeline functions (extract/generate/review) work through rig's trait objects
without knowing which provider is underneath.

---

### Stage 3 — SurrealDB RAG Store

**Files**: `src/rag.rs` (rewrite), `Cargo.toml`

**Goal**: Replace the in-memory `VectorStore` with SurrealDB embedded mode,
using `rig-surrealdb` for the vector store trait and keeping Ollama for
embedding generation.

#### 3a — SurrealDB Setup

1. **Add dependencies** to Cargo.toml (see above)

2. **Initialise SurrealDB embedded** in `RagEngine::new()`:
   ```rust
   // Persistent mode when output dir is available:
   use surrealdb::engine::local::RocksDb;
   let db = Surreal::new::<RocksDb>(surreal_path).await?;
   db.use_ns("dockock").use_db("rag").await?;

   // In-memory fallback:
   use surrealdb::engine::local::Mem;
   let db = Surreal::new::<Mem>(()).await?;
   ```

3. **Run migration** on startup:
   ```sql
   DEFINE TABLE IF NOT EXISTS chunks SCHEMAFULL;
   DEFINE FIELD IF NOT EXISTS document ON chunks TYPE object;
   DEFINE FIELD IF NOT EXISTS embedded_text ON chunks TYPE string;
   DEFINE FIELD IF NOT EXISTS embedding ON chunks TYPE array<float>;
   DEFINE INDEX IF NOT EXISTS chunk_embedding_idx ON chunks
       FIELDS embedding HNSW DIMENSION 768 DIST COSINE;
   ```
   (768 = nomic-embed-text dimensions)

#### 3b — Embedding via rig's EmbeddingModel trait

The `rig-surrealdb` `SurrealVectorStore` is generic over `Model: EmbeddingModel`.
rig's Ollama provider already implements `EmbeddingModel` for its client:

```rust
let ollama_client = rig::providers::ollama::Client::from_url("http://localhost:11435");
let embedding_model = ollama_client.embedding_model("nomic-embed-text");
// embedding_model implements rig::embeddings::EmbeddingModel
```

This means we can:
- Use rig's Ollama client for embeddings (local, port 11435)
- Pass the embedding model to `SurrealVectorStore::new()`
- SurrealDB handles storage + HNSW indexing + cosine retrieval

#### 3c — Replace RagEngine internals

| Current (in-memory) | New (SurrealDB) |
|---|---|
| `VectorStore { entries: Vec<EmbeddedChunk> }` | `SurrealVectorStore<RocksDb, OllamaEmbeddingModel>` |
| `embed_batch()` via raw HTTP `/api/embed` | `embedding_model.embed_documents()` via rig trait |
| `store.search(query_emb, exclude, top_k)` | `store.top_n(VectorSearchRequest::new(query, top_k))` |
| Manual cosine similarity | SurrealDB HNSW + `vector::similarity::cosine` |
| Flat scan O(n) | HNSW approximate O(log n) |
| Lost on process exit | Persisted to disk via RocksDB |

The `TextChunk` struct needs to implement `rig::Embed` + `Serialize` to work
with `InsertDocuments`. The `document` field in SurrealDB will store the
serialized `TextChunk`.

#### 3d — Cross-file exclusion filter

Current code excludes chunks from the query file:
`store.search(query_emb, exclude_file, TOP_K)`

With SurrealDB, use `SurrealSearchFilter`:
```rust
let filter = SurrealSearchFilter::does_not_contain(
    "document".into(),
    surrealdb::Value::from(file_name),
);
let req = VectorSearchRequest::new(query, TOP_K)
    .with_filter(filter);
store.top_n::<TextChunk>(req).await?
```

#### 3e — Disk cache integration

The current `DiskCache` caches embeddings by `(chunk_text, model)` hash.
With SurrealDB persisting to RocksDB, the embedding cache is handled by the
database itself — chunks are stored with their embeddings. On restart, no
re-embedding needed. The disk cache for embeddings (`NS_EMBEDDING`) becomes
redundant and can be removed.

**Deliverable**: RAG uses SurrealDB embedded mode with HNSW indexing, embeddings
via rig's Ollama trait, persistent across runs via RocksDB.

---

### Stage 4 — Raw HTTP Adaptation

**Files**: `src/llm/mod.rs`, `src/app.rs`

**Goal**: Replace remaining raw Ollama HTTP calls with provider-aware code.

1. **Vision (`describe_image_with_vision`)**: No change needed — always uses
   local Ollama via raw HTTP to `ENDPOINT_VISION` regardless of provider.

2. **Refinement (`app.rs` one-shot refine)**: Currently hits `/api/generate`
   directly. Rewrite to use rig-core's agent API:
   ```rust
   let agent = client.agent(model).preamble(preamble).build();
   let result = agent.prompt(prompt).await?;
   ```
   This works for both Ollama and OpenAI backends through the `LlmClient` enum.

3. **Warm-up**: Wrap in an `if matches!(backend, ProviderBackend::Ollama)` guard.
   Custom providers don't need warm-up.

4. **Prefix cache priming** (`app.rs` Phase 1.35): Wrap in an Ollama-only guard.
   When `generator_prefix_cache` is `None`, skip entirely.

**Deliverable**: All raw HTTP calls (except vision, which stays local) are
provider-aware.

---

### Stage 5 — UI & Configuration

**Files**: `src/app.rs`

**Goal**: Let the user switch between Ollama and the custom provider in the UI.

1. **Provider selector**: Add a dropdown at the top of the settings area:
   `[ Ollama (local) ▼ ]` / `[ rinf.tech AIArk (cloud) ▼ ]`
   Provider list populated from `custom_providers.json` + hardcoded "Ollama".

2. **Model dropdowns**: When custom provider is selected, populate generator /
   extractor / reviewer combo boxes from `custom_providers.json` models.
   Vision and Embedding stay fixed to local Ollama models.

3. **Connection status**: When custom provider is selected, show API key status:
   `"🔑 API key loaded"` or `"⚠ No API key — set AIARK_API_KEY in .env"`.
   Replace endpoint health indicators for the 3 cloud roles.

4. **Conditional UI elements**:
   - Hide concurrency slider for custom (hosted API handles load)
   - Hide PrefixCache status for custom
   - Keep vision endpoint status (still local)

5. **Persist selection**: Save the chosen provider + model assignments in the
   session state so they survive app restart.

**Deliverable**: Full UI for switching providers and selecting models.

---

### Stage 6 — Integration Testing & Polish

1. **Test Ollama path**: Ensure existing Docker-based Ollama workflow still works
   identically after all changes.

2. **Test Custom path**: Process a set of documents with the AIArk provider,
   verify extraction → generation → review pipeline produces valid Gherkin.

3. **Test SurrealDB persistence**: Process files, close app, reopen — verify
   embeddings are still in SurrealDB and cross-file context works without
   re-embedding.

4. **Test mixed mode**: Vision (local) + Generation (cloud) running
   simultaneously.

5. **Error handling**: API key missing, network timeout, model not found,
   rate limiting, SurrealDB corruption recovery.

---

## What We Keep

- ✅ **Vision**: Local Ollama `moondream` on port 11437 — unchanged
- ✅ **RAG retrieval logic**: Same chunking, same cross-file context building,
  same `build_query_text()` — only the storage layer changes
- ✅ **Pipeline structure**: Extract → Generate → Review → (optional OpenSpec)
  — identical flow regardless of provider
- ✅ **Disk caching**: LLM response cache (`NS_LLM`) still works — keyed by
  `(model, content_hash)`, works with any model name
- ✅ **All UI features**: Diff view, ratings, iterative refinement, file groups

## What We Lose (Custom Provider Only)

- ❌ **KV-cache prefix reuse**: Ollama-specific; disabled for custom providers
  (negligible — hosted APIs have no cold-start penalty)
- ❌ **Warm-up**: Unnecessary for hosted APIs
- ❌ **4-endpoint parallelism**: All 3 text roles share one cloud URL (the API
  handles parallelism server-side; we still use the semaphore for concurrency)

## What We Gain

- 🚀 **Massive context windows**: 120k-262k tokens vs 8k-32k locally —
  chunking almost never triggers
- 🚀 **Stronger models**: DeepSeek V3.2 + Kimi K2 Thinking >> qwen2.5-coder
- 🚀 **No GPU required**: Only vision needs local Ollama (1.7B model, runs on CPU)
- 🚀 **Persistent RAG**: SurrealDB + RocksDB survives restarts; no re-embedding
- 🚀 **HNSW indexing**: O(log n) retrieval vs O(n) brute-force

---

## Implementation Order

```
Stage 1 ──► Stage 2 ──► Stage 3 ──► Stage 4 ──► Stage 5 ──► Stage 6
Provider    Dual        SurrealDB   Raw HTTP    UI &        Testing
Abstraction Client      RAG Store   Adaptation  Config
            Construction
```

Stages 1-2 are tightly coupled (provider enum + client construction).  
Stage 3 (SurrealDB) is largely independent — can be built in parallel with Stage 2.  
Stage 4 is small (only refinement + guards).  
Stage 5 depends on everything else.  
Stage 6 is the final pass.

**Estimated total files changed**: 6 modified + 1 new  
- `Cargo.toml` — new deps  
- `src/main.rs` — dotenv init  
- `src/llm/provider.rs` — **new** — provider types + JSON parsing  
- `src/llm/mod.rs` — dual client, conditional prefix cache, agent building  
- `src/rag.rs` — SurrealDB replacement  
- `src/app.rs` — UI, provider selection, refinement rewrite, guards  
- `.env` — already created  
