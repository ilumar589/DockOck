# DockOck — Performance & Accuracy Improvement Plan

> Current baseline: No caching, no persistent state, no vector/embedding usage, raw text truncation at 12K chars, 400-char cross-file excerpts, full reprocessing every run.

---

## 1. Caching Layer

### 1.1 Parsed File Cache
**Impact:** Eliminates redundant parsing on repeat runs.

- Hash each input file (SHA-256 of bytes) → cache `ParseResult { text, images }` on disk
- Store in `<output_dir>/.dockock_cache/parsed/<hash>.bin` (bincode/MessagePack serialized)
- On load, if hash matches → skip parsing, load from cache
- Invalidation: automatic (content-hash based)

**Effort:** Low — add `serde::Serialize/Deserialize` to `ParseResult` and `ExtractedImage`, add a `cache` module.

### 1.2 LLM Response Cache
**Impact:** Avoids re-generating Gherkin for unchanged inputs. This is the biggest time-saver.

- Cache key = hash of: `(model_name, pipeline_mode, summary_text, cross_file_context_hash, preamble_version)`
- Store in `<output_dir>/.dockock_cache/llm/<hash>.json` with metadata (model, timestamp, token count)
- Serve cached Gherkin instantly instead of 30–120s LLM calls
- Add a "Force Regenerate" button/checkbox to bypass cache when needed

**Effort:** Medium — requires hashing the full prompt chain, serializing `GherkinDocument`.

### 1.3 Vision Description Cache
**Impact:** Image descriptions are expensive (moondream calls per image). Cache them.

- Cache key = SHA-256 of raw image bytes + vision model name
- Store in `<output_dir>/.dockock_cache/vision/<hash>.txt`
- Images rarely change between runs → very high cache hit rate

**Effort:** Low — straightforward key-value cache.

### 1.4 OpenSpec Artifact Cache
**Impact:** Avoids redundant OpenSpec generation for unchanged Gherkin.

- Cache key = hash of `(gherkin_text, generate_proposal flag)`
- Store cached response JSON
- Skip POST to OpenSpec service if cache hit

**Effort:** Low.

### 1.5 Implementation Sketch — Unified Cache Module

```rust
// src/cache.rs
pub struct DiskCache {
    base_dir: PathBuf,
}

impl DiskCache {
    pub fn new(output_dir: &Path) -> Self;
    pub fn get<T: DeserializeOwned>(&self, namespace: &str, key: &[u8]) -> Option<T>;
    pub fn put<T: Serialize>(&self, namespace: &str, key: &[u8], value: &T) -> Result<()>;
    pub fn invalidate(&self, namespace: &str, key: &[u8]);
    pub fn clear_namespace(&self, namespace: &str);
    pub fn clear_all(&self);
}

fn content_hash(data: &[u8]) -> [u8; 32]; // SHA-256
```

Add `sha2` crate for hashing, `bincode` or `rmp-serde` for serialization.

---

## 2. Vector Embeddings & Semantic Retrieval (RAG)

### 2.1 Problem with Current Cross-File Context
The current `build_summary()` uses a **fixed 400-char excerpt** per file. This is:
- Lossy — most document content is discarded
- Indiscriminate — the first 400 chars may be headers/boilerplate, not domain content
- Not semantically aware — no relevance ranking between files

### 2.2 Local Embedding Pipeline
**Impact:** Dramatically better cross-file context selection → higher quality Gherkin.

**Architecture:**
```
Parsed text ──► Chunk (512-token windows, 128-token overlap)
                    │
                    ▼
              Embed via Ollama embedding model (e.g., nomic-embed-text)
                    │
                    ▼
              Store vectors in local vector store (in-memory or on-disk)
                    │
                    ▼
     At generation time: embed the current file's key entities
              → retrieve top-K most relevant chunks from OTHER files
              → inject as cross-file context
```

**Model choice:** `nomic-embed-text` (137M params, fast, good quality) — can share the extractor Ollama instance.

**Vector store options:**
- **In-memory** (fastest, simplest): Use `rig-core`'s built-in `InMemoryVectorStore` or a simple sorted Vec with cosine similarity
- **On-disk** (persistent across sessions): `hnsw_rs` crate or `usearch` for approximate nearest neighbor
- **SQLite + vectors**: `sqlite-vec` extension for lightweight persistence

### 2.3 Smarter Context Window Construction
Instead of raw 400-char excerpts, use RAG to build the cross-file context:

```
Current file: D028.docx (being processed)
→ Extract key terms/entities from current file
→ Query vector store: "What chunks from OTHER files are most relevant?"
→ Retrieve top-8 chunks (ranked by cosine similarity, deduplicated)
→ Inject as structured context:

=== Related Context from Other Documents ===
[From D029.xlsx, Sheet "Requirements"] Relevant chunk here...
[From D031.vsdx, Page 2] Relevant chunk here...
```

**Effort:** Medium-High — requires embedding model deployment, chunking logic, vector store integration.

### 2.4 Gherkin Pattern Library (Few-Shot Examples)
**Impact:** Improve generation consistency and format quality.

- Maintain a local library of high-quality Gherkin examples (user-approved outputs)
- Embed each example
- At generation time, retrieve the 2-3 most semantically similar examples
- Inject as few-shot examples in the generator prompt

```
Here are examples of well-structured Gherkin for similar domains:

--- Example 1 (from: Invoice Processing) ---
Feature: Invoice Approval Workflow
  Scenario: Manager approves valid invoice
    Given an invoice with amount less than $10,000
    ...

--- Example 2 (from: User Onboarding) ---
...

Now generate Gherkin for the following document:
```

This creates a **self-improving feedback loop** — the more documents processed, the better the examples.

**Effort:** Medium — needs a "Save as Example" UI action, embedding, and prompt modification.

---

## 3. Prompt Engineering & Generation Accuracy

### 3.1 Structured Document Representation
**Current:** Raw text with headings stripped to Markdown-style `#`.
**Improved:** Convert to a structured intermediate format before LLM input.

```
=== DOCUMENT STRUCTURE ===
Title: Invoice Processing Workflow
Sections:
  1. Overview (paragraphs: 3, tables: 0)
  2. Requirements (paragraphs: 5, tables: 2)
  3. Process Flow (paragraphs: 2, diagrams: 1)

=== SECTION CONTENT ===
[Section 1: Overview]
...
[Section 2: Requirements]
Table 1: | Requirement | Priority | Status |
...
```

This helps the LLM understand document structure rather than treating it as flat text.

**Effort:** Medium — enhance `word::parse()` and `excel::parse()` to output structured metadata.

### 3.2 Semantic-Aware Truncation
**Current:** Hard truncation at 12,000 chars.
**Improved:** Priority-based truncation:

1. Extract document outline (headings, table headers) — always keep
2. Score sections by keyword density (requirements, process, rule, must, shall, when, if)
3. Truncate lowest-scoring sections first
4. Preserve table structures intact (don't split mid-table)

**Effort:** Medium — requires a scoring function in `preprocess_text()`.

### 3.3 Entity Extraction & Consistency Enforcement
**Impact:** Prevents LLM from inventing actors/entities that don't exist in source docs.

- After parsing, extract named entities (actors, systems, data objects) across all files
- Build a **project glossary** automatically
- Inject glossary into generator prompt:
  ```
  === PROJECT GLOSSARY ===
  Actors: Customer, Account Manager, Billing System
  Systems: SAP, CRM Portal, Invoice Gateway
  Data: Invoice, Purchase Order, Payment Record
  
  IMPORTANT: Use ONLY these terms in your Gherkin scenarios.
  ```

This is partially scaffolded already (`ProjectContext.entities`) but not implemented.

**Effort:** Low-Medium — regex/heuristic entity extraction from parsed text, prompt injection.

### 3.4 Adaptive Token Budgets
**Current:** Fixed 12K char limit for all documents.
**Improved:** Adjust based on:

- Document complexity (table count, section count, image count)
- Model context window (32b models handle more than 7b)
- Pipeline mode (Full has extraction step, so generator needs less raw text)

```rust
fn compute_token_budget(file_type: &str, complexity: &DocComplexity, model: &str) -> usize {
    let base = match model_context_window(model) {
        ..=4096 => 6_000,
        ..=8192 => 10_000,
        ..=32768 => 20_000,
        _ => 30_000,
    };
    // Reserve 30% for prompt template + cross-file context
    (base as f64 * 0.7) as usize
}
```

**Effort:** Low — parameterize `MAX_INPUT_CHARS`.

---

## 4. Pipeline & Concurrency Optimizations

### 4.1 Streaming Pipeline (Overlap Parsing + Generation)
**Current:** Phase 1 (parse ALL files) → Phase 2 (generate ALL).
**Improved:** Start generating as soon as the first file is parsed.

```
File 1: [Parse]──►[Generate]──►[Review]
File 2:    [Parse]───►[Generate]──►[Review]
File 3:       [Parse]────►[Generate]──►[Review]
```

Use `tokio::mpsc` to feed parsed results into a generation queue. Groups wait for all members to parse first.

**Impact:** Wall-clock time reduced by overlapping parsing and generation.
**Effort:** Medium — restructure `process_files()` from phased to streaming.

### 4.2 Incremental Processing
**Impact:** Only reprocess files that changed since last run.

- Track file modification times + content hashes
- On "Generate" click, compare against cached state
- Skip unchanged files, show cached results immediately
- Only send changed files through the LLM pipeline
- UI indicator: "3 of 7 files changed — processing only changed files"

Combines naturally with the caching layer (§1).

**Effort:** Low (with caching layer in place).

### 4.3 Adaptive Concurrency
**Current:** Fixed `MAX_CONCURRENT = 3`.
**Improved:** 
- Probe available VRAM/RAM on startup
- If all 4 Ollama instances are on separate GPUs → concurrency = 4
- If sharing a GPU → concurrency = 1-2 (prevent OOM)
- Expose as a UI slider for user override

**Effort:** Low — make `MAX_CONCURRENT` configurable, add UI control.

### 4.4 Model Warm-Up
**Impact:** First LLM call after container start is slow (model loading).
**Improvement:** After successful endpoint probe, send a minimal warm-up prompt:

```rust
async fn warm_up(client: &ollama::Client, model: &str) {
    let agent = client.agent(model).build();
    let _ = agent.prompt("Hi").await; // Forces model load
}
```

Run warm-ups in parallel for all 4 endpoints during the UI "checking Ollama..." phase.

**Effort:** Very Low.

### 4.5 Parallel Vision Processing
**Current:** Images are described sequentially in `enrich_text_with_images()`.
**Improved:** Use `futures::future::join_all()` to describe all images in parallel (with a small semaphore to avoid overwhelming the vision model).

**Effort:** Very Low.

---

## 5. Session Persistence & State Management

### 5.1 Project Session Save/Load
**Impact:** Resume interrupted work without re-processing.

Save to `<output_dir>/.dockock_session.json`:
```json
{
  "files": ["D028.docx", "D029.xlsx"],
  "groups": [{"name": "D028", "members": [...], "manual": false}],
  "results": { "D028.docx": { "gherkin": "...", "elapsed": 45.2 } },
  "models": { "generator": "qwen2.5-coder:32b", ... },
  "pipeline_mode": "Standard",
  "timestamp": "2026-03-11T..."
}
```

On app start, if session file exists → offer to restore.

**Effort:** Medium — serialize/deserialize `DockOckApp` state subset.

### 5.2 Result Diffing
When regenerating a file that has a cached result, show a diff view:
- Green = new scenarios added
- Red = scenarios removed
- Yellow = modified steps

Helps users understand what changed and verify improvements.

**Effort:** Medium — requires a diff algorithm and UI rendering.

---

## 6. Quality Feedback Loop

### 6.1 User Rating System
After generation, let users rate output quality (thumbs up/down per scenario or per file). Store alongside cached results.

Use ratings to:
- Prioritize which files need regeneration
- Populate the few-shot example library (§2.4) with highly-rated outputs
- Track quality metrics over time

**Effort:** Low (UI) + Low (storage).

### 6.2 Iterative Refinement
Allow users to select specific scenarios and request targeted improvements:
- "Make this more specific"
- "Add error handling scenarios"
- "Split this into multiple scenarios"

Send the current Gherkin + user instruction to the reviewer model for targeted editing.

**Effort:** Medium — new UI interaction + prompt construction.

---

## Priority Implementation Order

| Phase | Items | Impact | Effort |
|-------|-------|--------|--------|
| **Phase 1 — Quick Wins** | 4.4 Model Warm-Up, 4.5 Parallel Vision, 4.3 Adaptive Concurrency (UI slider) | Medium | Very Low |
| **Phase 2 — Caching** | 1.1 Parsed File Cache, 1.2 LLM Response Cache, 1.3 Vision Cache, 4.2 Incremental Processing | **Very High** | Low-Medium |
| **Phase 3 — Prompt Quality** | 3.2 Semantic Truncation, 3.3 Entity Glossary, 3.4 Adaptive Token Budgets | High | Medium |
| **Phase 4 — RAG Pipeline** | 2.1–2.3 Embedding + Vector Store + Semantic Context | **Very High** | Medium-High |
| **Phase 5 — Pipeline Optimization** | 4.1 Streaming Pipeline, 3.1 Structured Doc Representation | Medium-High | Medium |
| **Phase 6 — Feedback & Polish** | 2.4 Few-Shot Library, 5.1 Session Persistence, 6.1 Rating System, 6.2 Iterative Refinement, 5.2 Result Diffing | Medium | Medium |

---

## New Dependencies

| Crate | Purpose | Phase |
|-------|---------|-------|
| `sha2` | Content hashing for cache keys | 2 |
| `bincode` or `rmp-serde` | Binary serialization for cache | 2 |
| `rig-core` (embeddings) | Already included — use embedding API | 4 |
| `usearch` or manual cosine | Vector similarity search | 4 |

---

## Architecture After All Phases

```
                         ┌──────────────────────────────────┐
                         │          DiskCache               │
                         │  parsed/ │ vision/ │ llm/ │ spec/│
                         └─────┬────┴────┬────┴───┬──┴──┬──┘
                               │         │        │     │
.docx/.xlsx/.vsdx ──► [Parse] ─┤──► [Embed Chunks] ──► VectorStore
        (cached?)      │       │         │                   │
                       ▼       ▼         ▼                   │
                  ParseResult  Vision    Embeddings           │
                       │     Descriptions    │               │
                       ▼         │           ▼               │
                 [Preprocess] ◄──┘   [Retrieve Top-K] ◄─────┘
                       │          Cross-file chunks
                       ▼               │
                [Entity Extract] ──► Glossary
                       │               │
                       ▼               ▼
              ┌─────────────────────────────────┐
              │   LLM Generator Prompt          │
              │  • Structured summary           │
              │  • RAG cross-file context       │
              │  • Entity glossary              │
              │  • Few-shot examples            │
              └──────────────┬──────────────────┘
                             │ (cached?)
                             ▼
                    [LLM Generate → Review]
                             │
                             ▼
                      GherkinDocument
                       │         │
                   [Display]  [OpenSpec]
                       │         │ (cached?)
                    .feature   artifacts/
```
