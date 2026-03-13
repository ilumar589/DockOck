# DockOck — Workflow Document

> **Version**: 0.1.0  
> **Generated from source**: March 2026

DockOck is a native desktop application (Rust + egui) that parses business documents (Word, Excel, Visio) and transforms them into Gherkin `.feature` files using configurable LLM pipelines — locally via Ollama or through cloud-compatible APIs.

---

## Table of Contents

1. [High-Level Architecture](#1-high-level-architecture)
2. [Application Startup](#2-application-startup)
3. [User Interface Layout](#3-user-interface-layout)
4. [File Selection & Grouping](#4-file-selection--grouping)
5. [Configuration Options](#5-configuration-options)
6. [Processing Pipeline](#6-processing-pipeline)
   - [Phase 0 — Orchestrator Init & Endpoint Probing](#phase-0--orchestrator-init--endpoint-probing)
   - [Phase 1 — Parallel File Parsing](#phase-1--parallel-file-parsing)
   - [Phase 1.25 — Entity Extraction & Glossary](#phase-125--entity-extraction--glossary)
   - [Phase 1.35 — KV-Cache Prefix Priming](#phase-135--kv-cache-prefix-priming)
   - [Phase 2 — LLM Pipeline (per file/group)](#phase-2--llm-pipeline-per-filegroup)
   - [Phase 3 — OpenSpec Export (optional)](#phase-3--openspec-export-optional)
   - [Phase 4 — Session Persistence](#phase-4--session-persistence)
7. [LLM Pipeline Modes](#7-llm-pipeline-modes)
8. [Chunk-and-Merge for Large Documents](#8-chunk-and-merge-for-large-documents)
9. [Vision Model Integration](#9-vision-model-integration)
10. [Caching Strategy](#10-caching-strategy)
11. [RAG Cross-File Context](#11-rag-cross-file-context)
12. [Refinement Workflow](#12-refinement-workflow)
13. [Session Restore & Diffing](#13-session-restore--diffing)
14. [Provider Backends](#14-provider-backends)
15. [Error Handling & Resilience](#15-error-handling--resilience)
16. [Module Reference](#16-module-reference)

---

## 1. High-Level Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  DockOck Desktop App (Rust + eframe/egui)                    │
│                                                               │
│  ┌──────────────────┐   mpsc channel   ┌───────────────────┐ │
│  │  egui UI Loop    │◄────────────────│  Processing Thread │ │
│  │  (main thread)   │  ProcessingEvent │  (tokio runtime)   │ │
│  └──────────────────┘                  └───────────────────┘ │
│         │                                       │             │
│   rfd::FileDialog                    ┌──────────┴──────────┐ │
│         │                            │                     │ │
│         ▼                            ▼                     ▼ │
│  ┌────────────────┐          ┌──────────────┐    ┌────────┐ │
│  │ ProjectContext  │          │ File Parsers │    │ DiskCa │ │
│  │ (Arc<Mutex>)   │          │ word|excel|  │    │ (SHA-  │ │
│  └────────────────┘          │ visio        │    │  256)  │ │
│                              └──────────────┘    └────────┘ │
└──────────────────────────────────────────────────────────────┘
                         │ HTTP
           ┌─────────────┴──────────────────────┐
           │  LLM Backend (one of)               │
           │                                     │
           │  Ollama (local Docker):             │
           │    :11434 Generator                 │
           │    :11435 Extractor                 │
           │    :11436 Reviewer                  │
           │    :11437 Vision                    │
           │                                     │
           │  Custom (OpenAI-compatible cloud):  │
           │    Single API endpoint              │
           │    e.g. ByteDance AIArk             │
           └─────────────────────────────────────┘
```

**Threading model**: The egui event loop runs on the main thread. All LLM and file-processing work is spawned on a persistent Tokio multi-thread runtime via `std::thread::spawn` + `handle.block_on()`. Progress updates flow back to the UI via an `mpsc` channel carrying `ProcessingEvent` messages.

---

## 2. Application Startup

```
main()
 ├─ dotenv::dotenv()              ← Load .env (API keys, base URLs)
 ├─ tracing_subscriber::init()    ← Initialise logging (RUST_LOG env filter)
 ├─ tokio::runtime::Builder       ← Create multi-threaded async runtime
 │   └─ std::thread::spawn()      ← Park runtime in background thread
 ├─ eframe::NativeOptions         ← Window: 1100×700, min 700×450
 └─ eframe::run_native()          ← Launch UI with DockOckApp::new(handle)
      └─ DockOckApp::new()
           ├─ Load custom_providers.json (exe dir → cwd fallback)
           ├─ Set default models:
           │    Generator: qwen2.5-coder:32b
           │    Extractor: qwen2.5-coder:7b
           │    Reviewer:  qwen2.5-coder:7b
           │    Vision:    moondream
           ├─ Default pipeline: Fast
           ├─ Default concurrency: 3
           └─ Backend: Ollama (local)
```

---

## 3. User Interface Layout

```
┌──────────────────────────────────────────────┐
│  Top Bar                                      │
│  DockOck title | Ollama status | Output dir   │
│  Provider toggle | Model selectors            │
├─────────────────────┬────────────────────────┤
│  Left Panel         │  Right Panel            │
│  ➕ Add Files       │  Generated Gherkin      │
│  File list          │  📋 Copy  💾 Save       │
│  Groups             │  ✏ Refinement input     │
│  🗑 Clear           │  Diff toggle            │
│  ⭐ Ratings         │                         │
├─────────────────────┴────────────────────────┤
│  Bottom Bar                                   │
│  [⚙ Generate Gherkin] [⏹ Cancel]             │
│  Progress bar  |  Timestamped log panel       │
└──────────────────────────────────────────────┘
```

**Log levels** are colour-coded:
- Info (grey) — status updates
- Success (green) — completions
- Warning (yellow) — fallbacks
- Error (red) — failures

---

## 4. File Selection & Grouping

### Supported Formats

| Format | Extensions | Parser | Image Extraction |
|--------|-----------|--------|------------------|
| Word | `.docx` | ZIP + XML (`word/document.xml`) | Yes (`word/media/`) |
| Excel | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods` | calamine | No |
| Visio | `.vsdx`, `.vsd`, `.vsdm` | ZIP + XML (`visio/pages/pageN.xml`) | Yes (`visio/media/`) |

### Auto-Grouping

When `auto_group_enabled` is true (default), files with identical stems are automatically grouped:

```
D028_Req.docx  ┐
D028_Req.vsdx  ┤ → Group "D028_Req" (auto)
D028_Req.xlsx  ┘
```

- Auto-groups are rebuilt whenever the file list changes (`recompute_groups()`).
- Manual groups (user-created) are preserved across recomputation.
- Stale members (files removed from selection) are pruned automatically.
- Empty auto-groups are removed; empty manual groups are kept.

### Group Processing

Files within a group are **merged into a single concatenated text** and processed as one work item through the pipeline. This produces a single unified Gherkin feature that synthesizes all group members. Group-specific preambles (`GROUP_EXTRACTOR_PREAMBLE`, `GROUP_GENERATOR_PREAMBLE`) instruct the LLM to merge overlapping information.

---

## 5. Configuration Options

| Option | Default | Description |
|--------|---------|-------------|
| **Provider** | Ollama (local) | Ollama or Custom (cloud) — loaded from `custom_providers.json` |
| **Generator model** | `qwen2.5-coder:32b` | Primary LLM for Gherkin generation |
| **Extractor model** | `qwen2.5-coder:7b` | Structural analysis (Full mode only) |
| **Reviewer model** | `qwen2.5-coder:7b` | Gherkin quality review |
| **Vision model** | `moondream` | Image description (1.7B, CPU-friendly) |
| **Pipeline mode** | Fast | Fast / Standard / Full (see §7) |
| **Max concurrent** | 3 | Parallel LLM tasks (auto-adjusted to active endpoints) |
| **Force regenerate** | off | Bypass cache for all items |
| **Auto-group** | on | Group files by stem |
| **OpenSpec export** | off | Post-process Gherkin into OpenSpec artifacts |
| **Output directory** | — | Where `.feature` files and session are saved |

---

## 6. Processing Pipeline

Triggered by clicking **⚙ Generate Gherkin**. The full pipeline is orchestrated in the `process_files()` async function.

### Phase 0 — Orchestrator Init & Endpoint Probing

```
1. Probe Ollama endpoints (Generator, Extractor, Reviewer, Vision)
   - Generator (:11434) MUST be reachable (Ollama mode)
   - Others are optional with graceful fallbacks
2. Create AgentOrchestrator with appropriate clients
3. Start model warm-up (tokio::spawn) — runs concurrently with Phase 1
```

**Ollama mode**: Creates separate `ollama::Client` per reachable endpoint.  
**Custom mode**: Creates a single `openai::CompletionsClient` for all text roles; cloud vision via same API.

### Phase 1 — Parallel File Parsing

```
For each selected file (concurrent via tokio::task::spawn_blocking):
  1. Compute SHA-256 of file bytes → cache key
  2. Check NS_PARSED cache → if hit, return cached ParseResult
  3. If miss, parse based on extension:
     ├─ .docx → word::parse()  — headings, paragraphs, tables, images
     ├─ .xlsx → excel::parse() — sheet tabs, row data
     └─ .vsdx → visio::parse() — shapes, connectors, images
  4. Store ParseResult in cache
  5. Register FileContent in ProjectContext (Arc<Mutex>)
```

**Word parsing**: Extracts `<w:t>` text preserving heading levels as Markdown (`#`, `##`, ...), renders tables as pipe-delimited rows, extracts images from `word/media/`.

**Excel parsing**: Uses calamine for multi-format support, extracts per-sheet tab-separated content with workbook structure outline.

**Visio parsing**: Extracts shape labels, text, and connector relationships rendered as ASCII arrows (`Shape A → Shape B`), plus images from `visio/media/`.

### Phase 1.25 — Entity Extraction & Glossary

```
1. context.extract_entities() scans all parsed text:
   ├─ Pattern 1: Capitalised multi-word terms (2-5 words)
   │    e.g. "Invoice Processing System"
   └─ Pattern 2: Terms after actor signals
        ("the ", "user ", "system ", "service ", ...)
2. Noise filtering: common words, short terms, duplicates removed
3. Glossary injected into generator prompts for cross-file awareness
```

### Phase 1.35 — KV-Cache Prefix Priming

**Ollama only.** The generator's system prompt (preamble + glossary) is sent once to Ollama's `/api/generate` endpoint to pre-populate the KV-cache. Subsequent generation calls that share the same prefix skip re-computation (30–50% latency reduction).

```
PrefixCache:
  endpoint_url: "http://localhost:11434"
  model: "qwen2.5-coder:32b"
  cached_context: Vec<i64>   ← Ollama KV-cache token IDs
  prefix_hash: SHA-256 of (preamble + glossary)
```

### Phase 2 — LLM Pipeline (per file/group)

Work items are dispatched concurrently (bounded by semaphore = `max_concurrent`).

Each work item flows through:

```
┌─────────────┐
│ Parse Result │
│ (text+images)│
└──────┬──────┘
       ▼
┌──────────────┐
│ Vision Enrich│  ← Describe images (if any)
│  (optional)  │
└──────┬───────┘
       ▼
┌──────────────────────────────────────────────────────┐
│                 Pipeline Mode Switch                  │
├────────────┬─────────────────┬───────────────────────┤
│ Fast (1)   │ Standard (2)    │ Full (3)              │
│            │                 │                        │
│ Preprocess │ Preprocess      │ Extract (LLM)         │
│ (Rust)     │ (Rust)          │  ↓                    │
│  ↓         │  ↓              │ Generate (LLM)        │
│ Generate   │ Generate (LLM)  │  ↓                    │
│ (LLM)     │  ↓              │ Review (LLM)          │
│            │ Review (LLM)    │                        │
└────────────┴─────────────────┴───────────────────────┘
       ▼
┌─────────────┐
│ Parse LLM   │
│ output into  │
│ GherkinDoc  │
└──────┬──────┘
       ▼
 ProcessingEvent::FileResult / GroupResult → UI
```

**Cache check**: Before any LLM call, a composite key is computed from `(file_name, raw_text, mode, models, images_hash, context_summary)`. If a match exists in `NS_LLM`, the cached result is returned immediately.

### Phase 3 — OpenSpec Export (optional)

When `openspec_enabled` is true, after all Gherkin is generated:

```
For each GherkinDocument:
  1. POST /generate to OpenSpec service (localhost:11438)
     ├─ change_name: file stem
     ├─ gherkin: feature text
     └─ generate_proposal: true
  2. Receive artifacts: spec.md, tasks.md, design.md
  3. Save to <output_dir>/openspec/<change_name>/
  4. Send OpenSpecResult event → UI
```

### Phase 4 — Session Persistence

On pipeline completion:

```
SessionData {
  files, groups, results, group_results,
  ratings, models, pipeline_mode, max_concurrent,
  output_dir, previous_results, previous_group_results
}
 → serde_json::to_string_pretty()
 → <output_dir>/.dockock_session.json
```

---

## 7. LLM Pipeline Modes

### Fast (1 LLM call)

```
Raw text → preprocess_text() [Rust, zero-cost]
         → Generate (LLM)
         → Gherkin
```

`preprocess_text()` is a Rust function that truncates/structures the input within the model's character budget using line scoring:
- +10 for headings/section markers
- +8 for requirement keywords ("shall", "must", "require")
- +5 for tables (pipe-delimited)
- +4 for action keywords ("when", "if", "validate")
- +3 for actor/system keywords
- -2 for very short lines (<5 chars)

### Standard (2 LLM calls)

```
Raw text → preprocess_text() [Rust]
         → Generate (LLM)
         → Review (LLM)
         → Gherkin
```

Reviewer fixes syntax errors, ensures complete Given/When/Then steps, removes duplicates.

### Full (3 LLM calls)

```
Raw text → Extract (LLM) → structured summary
         → Generate (LLM) → Gherkin draft
         → Review (LLM) → final Gherkin
```

Extractor produces a structured summary with sections: ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES. Falls back to `preprocess_text()` on failure.

---

## 8. Chunk-and-Merge for Large Documents

When a document exceeds the model's input budget (`input_budget_for_model()`), it is split:

| Model Pattern | Budget (chars) | Approx. Tokens |
|---------------|---------------|-----------------|
| 128k models (qwen2.5-coder:32b) | 100,000 | ~25,000 |
| 32k models (deepseek, mixtral) | 48,000 | ~12,000 |
| 7b/8b models | 24,000 | ~6,000 |
| Default fallback | 12,000 | ~3,000 |

**Chunking strategy**:
- 20% overlap between chunks to preserve context continuity
- Snap to line boundaries (not mid-word)
- Each chunk processed through extract/preprocess → generate independently
- Final merge via `MERGE_REVIEWER_PREAMBLE`: combines all chunk Gherkin into a single cohesive feature, deduplicating scenarios

---

## 9. Vision Model Integration

For documents with embedded images:

```
For each ExtractedImage:
  1. Compute cache key: SHA-256(image_bytes + vision_model_name)
  2. Check NS_VISION cache → return description if hit
  3. If miss:
     ├─ Ollama: POST /api/generate with base64-encoded image
     │   model: moondream (or configured vision model)
     └─ Cloud: POST /chat/completions with image_url in message
  4. Cache description text
  5. Append description to document's raw text
```

**Vision prompt** instructs the model to describe: text/labels, diagram type, process flows, connections, tables, forms, UI wireframes — all focused on business-relevant information.

**Fallback chain**: Dedicated vision instance → Extractor instance → Generator instance.

---

## 10. Caching Strategy

All caching uses the `DiskCache` struct backed by SHA-256 content-addressed storage serialized with Postcard (binary) or plain text.

| Namespace | Key | Stores | Serialization |
|-----------|-----|--------|---------------|
| `NS_PARSED` | SHA-256(file_bytes) | `ParseResult` (text + images) | Postcard binary |
| `NS_VISION` | SHA-256(image_bytes + model) | Image description text | Plain text |
| `NS_LLM` | Composite(name, text, mode, models, images, context) | Final Gherkin text | Plain text |

**Cache location**: `<temp_dir>/dockock/.dockock_cache/<namespace>/` — uses local temp dir to avoid network I/O on mounted output directories.

**Invalidation**: Automatic when any input changes (content hash changes). Manual bypass via "Force Regenerate" button.

---

## 11. RAG Cross-File Context

The RAG module provides semantic cross-file context for multi-document processing:

- **Chunking**: Documents split into 2048-char chunks with 512-char overlap
- **Embedding**: Local TF-IDF feature hashing (FNV-1a, signed, 768-dim) — no API needed
- **Retrieval**: Brute-force cosine similarity, top-8 chunks, max 8000 chars context
- **Performance**: Sub-millisecond at typical scale (~3000 chunks × 768-dim)

> **Note**: RAG-based retrieval is currently disabled in favor of excerpt-based context (400-char per-file excerpts injected from `ProjectContext.build_summary()`).

---

## 12. Refinement Workflow

After initial generation, users can iteratively refine any result:

```
1. Select a file/group in the left panel
2. Type refinement instruction (e.g. "Add error handling scenarios")
3. Click ✏ Refine
4. Current Gherkin + instruction sent to generator model:
     ├─ Ollama: POST /api/generate
     └─ Cloud: agent.prompt() via rig-core
5. Refined Gherkin replaces current result
6. Previous result stored for diffing
```

The refinement prompt wraps the existing Gherkin and user instruction, asking the model to output only the complete revised feature file.

---

## 13. Session Restore & Diffing

### Session Persistence

- **File**: `<output_dir>/.dockock_session.json`
- **Saved on**: Pipeline completion (auto), explicit save
- **Restored on**: App startup if output directory contains a session file

### Diffing

`session::diff_gherkin()` computes LCS-based (Longest Common Subsequence) line-level diffs:
- `DiffLine::Unchanged` — line present in both versions
- `DiffLine::Added` — new line in current version
- `DiffLine::Removed` — line from previous version

Previous results are stored in `previous_results` / `previous_group_results` after each regeneration. The "Diff" toggle in the right panel shows inline changes.

---

## 14. Provider Backends

### Ollama (Local)

```rust
ProviderBackend::Ollama
```

- Separate Docker containers per role (generator :11434, extractor :11435, reviewer :11436, vision :11437)
- GPU support via nvidia drivers (falls back to CPU)
- KV-cache prefix reuse for shared system prompts
- Model warm-up on pipeline start

### Custom (OpenAI-Compatible Cloud)

```rust
ProviderBackend::Custom { name, base_url, api_key }
```

- Single `openai::CompletionsClient` for all text roles
- Cloud vision via same API (e.g. ByteDance AIArk `seed-2-0-lite`)
- Configured via `custom_providers.json` + `.env` for API keys
- No KV-cache or warm-up optimisations (not applicable)
- HTTP timeouts: 30s connect, 90s read (streaming not capped)

### Custom Providers JSON

```json
{
  "provider": {
    "<key>": {
      "name": "Display Name",
      "options": { "baseURL": "https://..." },
      "models": {
        "<model_id>": {
          "name": "Display Name",
          "limit": { "context": 120000, "output": 32768 }
        }
      },
      "defaults": {
        "generator": "<model_id>",
        "extractor": "<model_id>",
        "reviewer": "<model_id>",
        "vision": "<model_id>"
      }
    }
  }
}
```

API key is loaded from environment variables (e.g. `AIARK_API_KEY`).

---

## 15. Error Handling & Resilience

| Situation | Strategy |
|-----------|----------|
| Generator offline (Ollama) | **Fatal** — pipeline cannot start |
| Extractor offline | Fall back to generator for extraction |
| Reviewer offline | Skip review, return unreviewed Gherkin |
| Vision offline | Fall back to extractor → generator, or skip |
| LLM extraction fails | Fall back to `preprocess_text()` (Rust) |
| LLM review fails | Return unreviewed Gherkin |
| File parse fails | `ItemFailed` event, skip file, continue others |
| Cloud API 429/503 | Retry with exponential backoff (3 attempts: 5s/15s/30s) |
| Streaming stall | 60s per-chunk timeout, terminate and retry |
| Overall request timeout | 120–240s depending on pipeline stage |
| User cancellation | `cancel_flag` (AtomicBool) checked between work items |
| Tokio task panic | Caught at `handle.await`, logged, pipeline continues |

All errors are surfaced as timestamped, colour-coded log entries in the UI.

---

## 16. Module Reference

| Module | File | Responsibility |
|--------|------|----------------|
| **main** | `src/main.rs` | Entry point: Tokio runtime, eframe window init |
| **app** | `src/app.rs` | UI state (`DockOckApp`), event loop, `process_files()` orchestration |
| **context** | `src/context.rs` | `ProjectContext`, `FileGroup`, entity extraction, cross-file summaries |
| **gherkin** | `src/gherkin.rs` | `GherkinDocument`, `Scenario`, `Step` structs + LLM output parser |
| **parser** | `src/parser/mod.rs` | `ParseResult`, `ExtractedImage`, format dispatch |
| **parser::word** | `src/parser/word.rs` | `.docx` ZIP/XML parsing (paragraphs, headings, tables, images) |
| **parser::excel** | `src/parser/excel.rs` | `.xlsx`/`.xls`/`.ods` via calamine (sheets, rows, structure) |
| **parser::visio** | `src/parser/visio.rs` | `.vsdx` ZIP/XML parsing (shapes, connectors, images) |
| **llm** | `src/llm/mod.rs` | `AgentOrchestrator`, pipeline modes, preambles, streaming, chunking |
| **llm::provider** | `src/llm/provider.rs` | `ProviderBackend`, `CustomProviderConfig`, JSON config loading |
| **llm::prefix_cache** | `src/llm/prefix_cache.rs` | Ollama KV-cache reuse for shared prompt prefixes |
| **cache** | `src/cache.rs` | `DiskCache` — SHA-256 content-addressed, Postcard/text serialization |
| **rag** | `src/rag.rs` | TF-IDF hashing embeddings, chunking, brute-force cosine retrieval |
| **session** | `src/session.rs` | `SessionData`, JSON persistence, LCS-based Gherkin diffing |
| **openspec** | `src/openspec.rs` | HTTP client for OpenSpec service (`/health`, `/generate`) |

### Key Dependencies

| Crate | Purpose |
|-------|---------|
| `eframe` / `egui` | Native desktop UI |
| `rig-core` 0.32.0 | LLM integration (Ollama + OpenAI providers) |
| `tokio` 1.50.0 | Async runtime (full features) |
| `calamine` | Excel/ODS parsing |
| `zip` + `roxmltree` | Word/Visio XML extraction |
| `reqwest` | HTTP client for LLM APIs and OpenSpec |
| `serde` + `serde_json` | JSON serialization |
| `postcard` | Binary serialization for cache |
| `sha2` | SHA-256 hashing for content-addressed caching |
| `rfd` | Native file dialogs |
| `base64` | Image encoding for vision models |
| `regex` | Text pattern matching |

---

## End-to-End Example Workflows

### Single File — Fast Mode

```
1. User launches DockOck
2. Provider: Ollama, Pipeline: Fast, Model: qwen2.5-coder:32b
3. Click ➕ Add Files → select invoice_process.docx
4. Click ⚙ Generate Gherkin
   ├─ Probe Ollama → Generator online ✓
   ├─ Warm up model (concurrent)
   ├─ Parse .docx → text + 2 images
   ├─ Extract entities → "Invoice Processing", "Accounts System"
   ├─ Vision: describe 2 images → append descriptions to text
   ├─ Preprocess text (Rust, instant)
   ├─ Generate Gherkin (1 LLM call, ~15-45s)
   └─ Done ✓
5. Right panel: Feature: Invoice Processing …
6. Click 📋 Copy or 💾 Save
```

### Multi-File Project — Full Mode with Groups

```
1. Add 3 files: D028_Req.docx, D028_Flow.vsdx, D028_Data.xlsx
2. Auto-group: "D028_Req" ← D028_Req.docx
   (D028_Flow.vsdx, D028_Data.xlsx ungrouped or manually grouped)
3. Pipeline: Full, Max concurrent: 2
4. Click ⚙ Generate Gherkin
   ├─ Parse all 3 files in parallel
   ├─ Extract entities from all
   ├─ Prime KV-cache prefix
   ├─ Process group "D028_Req" (merged text):
   │    Extract (LLM) → Generate (LLM) → Review (LLM)
   ├─ Process D028_Flow.vsdx (ungrouped):
   │    Extract → Generate → Review
   ├─ Process D028_Data.xlsx (ungrouped):
   │    Extract → Generate → Review
   └─ Done — 3 Gherkin documents
5. Review each result, rate with 👍/👎
6. Refine: "Add validation scenarios for data model" → ✏ Refine
7. Save All → .feature files + session
```
