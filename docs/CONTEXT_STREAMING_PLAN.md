# Context Streaming & Optimization Plan

## Overview

Three optimizations to reduce redundant compute, improve prompt quality, and
handle documents that exceed model context windows.

| # | Feature | Impact | Complexity |
|---|---------|--------|------------|
| 1 | KV-cache prefix reuse | ~30-50% faster batches | Medium |
| 2 | Multi-turn chat format | Better prompt structure | Low |
| 3 | Chunk-and-merge | Handle oversized docs | Medium |

---

## Feature 1 — Ollama KV-cache Prefix Reuse

### Problem

Every file processed through the pipeline sends the full system prompt +
glossary + cross-file context to the model. Ollama recomputes attention over
this shared prefix for every request, wasting significant GPU cycles.

### How Ollama KV-cache works

Ollama's `/api/generate` endpoint returns a `context` field — an opaque token
array representing the KV-cache state after processing the prompt. If you send
this `context` back in a subsequent request, Ollama skips reprocessing the
cached prefix tokens and jumps straight to the new content.

**Important**: This only works via `/api/generate`, NOT `/api/chat`.
rig-core's Ollama provider uses `/api/chat`, so we must bypass rig-core
for prefix-cached calls and use raw `reqwest` HTTP.

### Design

#### Phase 1a — Shared-prefix cache for the Generator stage

The Generator stage is the most expensive (largest model, longest prompts). All
Generator calls share: `GENERATOR_PREAMBLE` (system) + `PROJECT GLOSSARY`.

```
Prefix (cached):
  system: GENERATOR_PREAMBLE
  prompt: "=== PROJECT GLOSSARY ===\n{glossary}\n"

Per-file suffix (new each call):
  "{context_section}\n=== Structured Summary ===\n{summary}\nGenerate the Gherkin..."
```

#### New struct: `PrefixCache`

```rust
// src/llm/prefix_cache.rs (new file)

/// Holds a cached Ollama KV-cache prefix for a given (endpoint, model, prefix_text) triple.
pub struct PrefixCache {
    endpoint_url: String,
    model: String,
    client: reqwest::Client,
    /// The cached context token array from Ollama, if primed.
    cached_context: Option<Vec<i64>>,
    /// Hash of the prefix text used to detect invalidation.
    prefix_hash: Option<String>,
}

impl PrefixCache {
    pub fn new(endpoint_url: &str, model: &str) -> Self { ... }

    /// Prime the cache by sending the shared prefix to Ollama.
    /// Ollama processes it, returns the KV-cache state, and we store it.
    pub async fn prime(&mut self, prefix_text: &str) -> Result<()> {
        // POST /api/generate { model, prompt: prefix_text, stream: false }
        // Store response.context + hash of prefix_text
    }

    /// Generate with the cached prefix. Sends the suffix + cached context.
    /// Returns (response_text, updated_context).
    pub async fn generate_with_prefix(
        &self,
        suffix: &str,
        options: serde_json::Value,  // includes num_ctx
    ) -> Result<String> {
        // POST /api/generate {
        //   model, prompt: suffix, context: self.cached_context,
        //   system: "", options, stream: false
        // }
    }

    /// Streaming variant — returns chunks via a channel.
    pub async fn stream_generate_with_prefix(
        &self,
        suffix: &str,
        options: serde_json::Value,
        status_tx: &std::sync::mpsc::Sender<String>,
        stage_name: &str,
        file_name: &str,
        timeout: std::time::Duration,
    ) -> Result<String> {
        // POST /api/generate {
        //   model, prompt: suffix, context: self.cached_context,
        //   system: "", options, stream: true
        // }
        // Read SSE stream, accumulate text, send progress
    }

    /// Check if cache is valid for the given prefix.
    pub fn is_valid_for(&self, prefix_text: &str) -> bool {
        // Compare sha256(prefix_text) with stored prefix_hash
    }

    /// Clear the cache (e.g., when glossary changes between runs).
    pub fn invalidate(&mut self) { ... }
}
```

#### Changes to `AgentOrchestrator`

```rust
pub struct AgentOrchestrator {
    // ... existing fields ...
    /// KV-cache for generator shared prefix (glossary + preamble)
    generator_prefix_cache: Option<PrefixCache>,
    /// KV-cache for extractor shared prefix (preamble only, Full mode)
    extractor_prefix_cache: Option<PrefixCache>,
}
```

#### Changes to `generate()`

```
Before:
  1. Build agent with preamble + additional_params
  2. Build prompt = context_section + glossary + summary
  3. stream_with_progress(agent, prompt)

After:
  1. If prefix_cache is primed and valid:
     a. suffix = context_section + summary (NO glossary — it's in the prefix)
     b. stream_generate_with_prefix(suffix, options)
  2. Else:
     a. Fall back to current rig-core agent approach
```

#### Changes to `process_files()` (app.rs orchestration)

```
After Phase 1.25 (extract_entities / build_glossary):
  1. Build prefix_text = GENERATOR_PREAMBLE + "\n" + glossary
  2. orchestrator.generator_prefix_cache.prime(prefix_text).await
  3. If Full mode:
     a. Build extractor prefix = EXTRACTOR_PREAMBLE
     b. orchestrator.extractor_prefix_cache.prime(extractor_prefix).await
```

#### New response struct

```rust
#[derive(serde::Deserialize)]
struct OllamaGenerateResponseFull {
    response: String,
    /// Opaque KV-cache token state — feed back to skip prefix recomputation.
    context: Option<Vec<i64>>,
}
```

#### Files changed

| File | Changes |
|------|---------|
| `src/llm/prefix_cache.rs` | **New file** — `PrefixCache` struct and methods |
| `src/llm/mod.rs` | Add `mod prefix_cache;`, add `PrefixCache` fields to orchestrator, modify `generate()` and `extract()` to use prefix cache when available, add `OllamaGenerateResponseFull` |
| `src/app.rs` | Prime prefix caches in `process_files()` after glossary extraction |

#### Constraints

- The prefix cache is per-endpoint, per-model. If the user changes the
  generator model mid-run (they can't currently), the cache would need
  invalidation.
- Ollama's KV-cache is stored in GPU VRAM. Priming uses memory but saves
  recomputation for all subsequent calls.
- The prefix must be identical byte-for-byte. The glossary is deterministic
  for a given file set, so this is safe within a single run.
- We do NOT use prefix cache for the Reviewer stage — its input varies
  completely per file (just the Gherkin output), so there's no shared prefix
  worth caching.

---

## Feature 2 — Multi-turn Chat Format

### Problem

Currently, all context (system prompt, glossary, cross-file context, document
content) is concatenated into a single giant user message string. The model
treats this as undifferentiated text, which can cause:

- The model losing track of what's instruction vs. content
- Poor attention distribution over long prompts
- No clear delineation between "here's context" and "here's the task"

### Design

Replace `stream_with_progress(agent, single_prompt)` with
`stream_chat_with_progress(agent, prompt, chat_history)` using rig-core's
`agent.stream_chat(prompt, chat_history)`.

#### New helper: `stream_chat_with_progress()`

```rust
async fn stream_chat_with_progress<M, P>(
    agent: &rig::agent::Agent<M, P>,
    prompt: &str,
    chat_history: Vec<Message>,
    stage_name: &str,
    file_name: &str,
    status_tx: &std::sync::mpsc::Sender<String>,
    timeout: std::time::Duration,
) -> Result<String>
```

Same as `stream_with_progress` but calls `agent.stream_chat(prompt, history)`
instead of `agent.stream_prompt(prompt)`.

#### Message structure per stage

**Extract stage (Full mode):**
```
System (preamble): EXTRACTOR_PREAMBLE
History:
  User[0]: "Here is the document metadata:\nFile: {file_name}\nType: {file_type}"
  User[1]: "=== Document Content ===\n{raw_text}"
Prompt:  "Produce the structured summary now."
```

**Generate stage:**
```
System (preamble): GENERATOR_PREAMBLE
History:
  User[0]: "=== PROJECT GLOSSARY ===\n{glossary}"
  User[1]: "=== Related Context from Other Documents ===\n{context_section}"
  User[2]: "=== Structured Summary ===\n{summary}"
Prompt:  "Generate the Gherkin Feature for document: {file_name}"
```

**Review stage:**
```
System (preamble): REVIEWER_PREAMBLE
History:
  (none — the Gherkin is the only input, keep it simple)
Prompt:  "Review and correct:\n{gherkin}\nCorrected Gherkin:"
```

#### Interaction with Feature 1 (KV-cache)

Features 1 and 2 are **mutually exclusive per call**:
- If prefix cache is available and primed → use Feature 1 (raw HTTP, no
  rig-core agent, but still streams output)
- If prefix cache is NOT available → use Feature 2 (rig-core stream_chat
  with structured messages)
- Feature 2 is the fallback and applies to ALL stages; Feature 1 only
  applies to Generator and optionally Extractor

The priority order: Feature 1 > Feature 2 > current single-prompt approach.

#### Files changed

| File | Changes |
|------|---------|
| `src/llm/mod.rs` | Add `stream_chat_with_progress()` helper, restructure `extract()`, `generate()`, `review()`, `extract_group()`, `generate_group()` to build `Vec<Message>` chat history instead of single prompt string |

#### Imports needed

```rust
use rig::completion::message::Message;  // User/Assistant variants
use rig::streaming::StreamingChat;      // for stream_chat()
```

---

## Feature 3 — Chunk-and-Merge for Oversized Documents

### Problem

Documents that exceed the model's context window (after `num_ctx` is set) are
silently truncated by `preprocess_text()` / `input_budget_for_model()`. Large
Visio diagrams, multi-sheet Excel workbooks, or long Word documents can lose
critical content from later sections.

### Design

#### Detection

```rust
fn needs_chunking(text: &str, model: &str) -> bool {
    text.len() > input_budget_for_model(model)
}
```

#### Chunking strategy

Split the document into overlapping windows sized to fit within the model's
input budget, respecting section/paragraph boundaries:

```rust
fn chunk_for_llm(text: &str, model: &str) -> Vec<LlmChunk> {
    let budget = input_budget_for_model(model);
    let overlap = budget / 5;  // 20% overlap for continuity
    // Split at paragraph/section boundaries
    // Each chunk gets: chunk_index, total_chunks, text
}

struct LlmChunk {
    index: usize,
    total: usize,
    text: String,
}
```

#### Per-chunk processing

Each chunk goes through the normal pipeline (extract/generate), but with a
modified prompt that tells the model this is chunk N of M:

```
Extract prompt:
  "This is part {index+1} of {total} of the document '{file_name}'.
   Focus on extracting information from THIS section.
   Previous sections covered: {prev_summary_hint}
   === Document Content (Part {index+1}/{total}) ===
   {chunk_text}
   Structured summary for this section:"

Generate prompt:
  "This is part {index+1} of {total} for document '{file_name}'.
   === Context from other parts ===
   {summaries_from_other_chunks}
   === Structured Summary (Part {index+1}/{total}) ===
   {chunk_summary}
   Generate Gherkin scenarios for THIS section only:"
```

#### Merge strategy

After all chunks produce Gherkin, merge them:

```rust
fn merge_chunk_gherkin(
    file_name: &str,
    chunks: Vec<(usize, String)>,  // (chunk_index, gherkin_text)
    orchestrator: &AgentOrchestrator,
    status_tx: &Sender<String>,
) -> Result<String> {
    // 1. Parse each chunk's Gherkin
    // 2. Concatenate all scenarios under a single Feature
    // 3. Send to reviewer with merge-specific prompt:
    //    "The following Gherkin was generated from {N} sections of the same
    //     document. Merge into a single cohesive Feature:
    //     - Remove duplicate scenarios
    //     - Unify Background steps
    //     - Ensure consistent naming
    //     - Preserve all unique scenarios"
}
```

#### Integration into `process_file()`

```
Current flow:
  preprocess/extract → generate → review

New flow:
  IF needs_chunking(raw_text, generator_model):
    chunks = chunk_for_llm(raw_text, extractor_or_generator_model)
    FOR each chunk (concurrent, respecting semaphore):
      summary_i = extract/preprocess(chunk)
    FOR each chunk (sequential — needs prior summaries for context):
      gherkin_i = generate(chunk_summary, other_summaries_hint)
    merged = merge_chunk_gherkin(all_gherkin)
    final = review(merged)         // single review pass on merged result
  ELSE:
    (existing flow unchanged)
```

#### Progress reporting

Each chunk reports progress with chunk indicators:
```
"🔄 [Extract] document.docx [2/5]: 140 tokens…"
"✓ [Generate] document.docx [3/5]: done (423 tokens)"
"🔄 [Merge Review] document.docx: merging 5 chunks…"
```

#### Files changed

| File | Changes |
|------|---------|
| `src/llm/mod.rs` | Add `needs_chunking()`, `chunk_for_llm()`, `LlmChunk`, `merge_chunk_gherkin()`. Modify `process_file()` to detect oversized docs and route through chunk pipeline. Add merge-reviewer preamble constant. |
| `src/app.rs` | No changes — chunking is transparent to the UI. `ProcessingEvent::FileResult` works the same. |

#### Constraints

- Chunks should split at paragraph/section boundaries, not mid-sentence.
  Reuse the `snap_to_line_boundary()` logic from `rag.rs`.
- The merge review step is critical — without it, chunks produce
  overlapping/contradictory scenarios.
- Chunk extraction can run in parallel (they're independent). Chunk
  generation should run sequentially so each chunk can reference prior
  summaries as context hints.
- The overlap between chunks (20%) helps the model maintain continuity
  across boundaries.
- This does NOT affect groups — `process_group()` already merges member
  texts within a budget. If the merged group text is still too large,
  chunking would apply to the merged result.

---

## Implementation Order

```
Phase A — Feature 2 (Multi-turn chat)        [Low complexity, no new files]
  ├─ Add stream_chat_with_progress() helper
  ├─ Restructure extract/generate/review to use chat history
  ├─ Same for extract_group/generate_group
  └─ cargo check + test

Phase B — Feature 3 (Chunk-and-merge)        [Medium, self-contained]
  ├─ Add LlmChunk, needs_chunking(), chunk_for_llm()
  ├─ Add merge_chunk_gherkin() with merge-reviewer preamble
  ├─ Wire into process_file() with oversized-doc detection
  ├─ Wire into process_group() for oversized merged groups
  └─ cargo check + test with large document

Phase C — Feature 1 (KV-cache prefix reuse)  [Medium, new file + raw HTTP]
  ├─ Create src/llm/prefix_cache.rs
  ├─ Add PrefixCache fields to AgentOrchestrator
  ├─ Prime caches in process_files() after glossary extraction
  ├─ Modify generate() to use prefix cache when available
  ├─ Optionally modify extract() (Full mode only)
  ├─ Add streaming support for raw HTTP prefix-cached generation
  └─ cargo check + test batch of files to verify speedup
```

**Rationale**: Feature 2 first because it's the simplest and improves prompt
quality for all subsequent work. Feature 3 next because it's a correctness
fix (prevents data loss on large docs). Feature 1 last because it's a pure
performance optimization and is the most complex (raw HTTP, cache
invalidation).

---

## Testing Strategy

| Feature | Test approach |
|---------|---------------|
| Multi-turn chat | Process same files before/after, compare Gherkin quality. Verify via Ollama logs that requests use proper message array structure. |
| Chunk-and-merge | Create/select a document >100K chars. Verify it produces Gherkin covering content from all sections, not just the first N chars. |
| KV-cache | Process 5+ files in a batch. Compare total wall-clock time vs. without cache. Verify via status messages that "prefix cache primed" appears once and subsequent files skip prefix processing. |
