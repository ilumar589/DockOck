# Feature: Context-Only Files (Excel & Visio)

## Problem

Currently every imported file — Word, Excel, and Visio — is independently
transformed into a Gherkin `.feature` file.  In practice, **only Word documents
describe requirements** that should become Gherkin scenarios.  Excel spreadsheets
(field lists, status matrices, configuration tables) and Visio diagrams
(workflows, architecture, state machines) provide **reference context** that
should inform the Word-to-Gherkin transformation but should **not** produce
their own Gherkin output.

## Desired Behaviour

| File type | Role | Gherkin output? |
|-----------|------|-----------------|
| `.docx` | **Primary** – requirements document | ✅ Yes |
| `.xlsx` / `.xls` / `.xlsm` / `.xlsb` / `.ods` | **Context** – reference data | ❌ No |
| `.vsdx` / `.vsd` / `.vsdm` | **Context** – reference diagrams | ❌ No |

- Context files are still **parsed** (text + images extracted).
- Their content is injected into the LLM prompt when processing Word files.
- They do **not** produce a `FileResult` / `GroupResult` of their own.
- The UI visually distinguishes primary vs context files.

---

## Implementation Plan

### 1. Data Model — introduce a file-role concept

**File: `src/parser/mod.rs`**

Add a `FileRole` enum alongside the existing `ParseResult`:

```rust
/// Whether a file produces its own Gherkin or only provides context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    /// Transformed into Gherkin (Word documents).
    Primary,
    /// Parsed for context only; no Gherkin output (Excel, Visio).
    Context,
}

impl FileRole {
    /// Derive the role from a file extension.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "docx" => Self::Primary,
            _      => Self::Context,   // xlsx, vsdx, etc.
        }
    }
}
```

Extend `ParseResult` with the role:

```rust
pub struct ParseResult {
    pub file_type: String,
    pub text: String,
    pub images: Vec<ExtractedImage>,
    pub role: FileRole,               // ← NEW
}
```

Set `role` inside `parse_file()` based on the extension that was already matched.

### 2. Context Assembly — always include context files

**File: `src/context.rs`**

Extend `FileContent` with the role:

```rust
pub struct FileContent {
    pub path: PathBuf,
    pub file_type: String,
    pub raw_text: String,
    pub role: FileRole,               // ← NEW
}
```

Add a helper to `ProjectContext`:

```rust
impl ProjectContext {
    /// Build a combined context string from all context-only files
    /// (Excel, Visio).  This is injected into every primary-file prompt
    /// in addition to the normal cross-file summary.
    pub fn build_context_only_summary(&self) -> String {
        self.file_contents
            .values()
            .filter(|fc| fc.role == FileRole::Context)
            .map(|fc| {
                let name = fc.path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                format!("=== Reference: {} ({}) ===\n{}", name, fc.file_type, fc.raw_text)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
```

This summary is always appended to the prompt of every primary file, ensuring
Excel/Visio content is available even when cross-file context is truncated.

### 3. Pipeline — skip Gherkin generation for context files

**File: `src/app.rs` — `process_files()`**

#### 3a. Phase 1 (parsing) — no change

All files continue to be parsed.  The `ParseResult::role` is stored alongside
the text and images in `parsed_map`.

#### 3b. Phase 1.5 (work-item creation) — filter out context files

When constructing work items from ungrouped files (≈ line 2745):

```rust
for (path, (file_type, raw_text, images, role)) in &parsed_map {
    if grouped_paths.contains(path) { continue; }
    if *role == FileRole::Context    { continue; }  // ← NEW: skip context files
    // ... spawn process_file task as before ...
}
```

For groups, two sub-cases:

- **All members are context-only** → skip the group entirely, emit a
  `Status("ℹ Group X contains only context files — skipped")` event.
- **Mixed group** (Word + Excel/Visio) → pass the whole group to
  `process_group()` as today.  The group preamble already merges all members,
  so context files naturally provide reference.  The resulting Gherkin is
  attributed to the group as a whole.
- **Single-member context file in a group** → same skip logic.

#### 3c. Emit a status event for context files

So the user knows context files were recognized:

```rust
if *role == FileRole::Context {
    let _ = tx.send(ProcessingEvent::Status(format!(
        "📎 {} loaded as reference context", file_name
    )));
    continue;
}
```

### 4. LLM Prompts — surface context-file content explicitly

**File: `src/llm/mod.rs`**

In `generate()` and `generate_group()`, inject the context-only summary as a
dedicated chat-history message (separate from cross-file context so the model
sees it as a distinct section):

```rust
// After building the existing chat_history vec:
let ctx_summary = context.build_context_only_summary();
if !ctx_summary.is_empty() {
    chat_history.push(Message::user(format!(
        "=== REFERENCE DATA (Excel / Visio — do NOT generate scenarios for these) ===\n{}",
        ctx_summary
    )));
}
```

This labels the data clearly so the LLM knows these are reference materials,
not requirements to convert.

### 5. UI Changes

**File: `src/app.rs`**

#### 5a. File list — visual distinction

In `render_left_panel()`, when rendering ungrouped files, show a different icon
and muted colour for context files:

| State | Primary file | Context file |
|-------|-------------|--------------|
| Pending | `📄 FileName.docx` | `📎 FileName.xlsx` (dimmed) |
| Done | `✔ FileName.docx` | `📎 FileName.xlsx` (dimmed, no ✔) |
| Failed | `✖ FileName.docx` | _(context files don't fail)_ |

Implementation: derive the role from the file extension at render time
(`FileRole::from_extension`), then branch on it:

```rust
let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
let role = crate::parser::FileRole::from_extension(ext);

if role == crate::parser::FileRole::Context {
    // Context file — show clip icon, dimmed, not selectable for output
    let label = egui::RichText::new(format!("📎 {}", name))
        .color(egui::Color32::from_rgb(140, 140, 160));
    ui.label(label)
        .on_hover_text(format!("{} (reference context — no Gherkin output)", path.display()));
} else {
    // Primary file — existing rendering (✔ / ✖ / plain)
    // ... unchanged ...
}
```

#### 5b. Context file — click behaviour

Selecting a context file shows a **read-only info panel** in the right pane
instead of Gherkin output:

```
╔══════════════════════════════════════════╗
║  📎 ITP_STT_STATUS.xlsx                  ║
║  Type: Excel · Role: Reference Context   ║
║                                          ║
║  This file provides reference data to    ║
║  Word document processing — no Gherkin   ║
║  scenarios are generated for it.         ║
║                                          ║
║  ┌─ Preview (first 50 lines) ─────────┐ ║
║  │ Status | Code | Description | ...   │ ║
║  │ ACTIVE | ACT  | Active record  ...  │ ║
║  │ ...                                 │ ║
║  └─────────────────────────────────────┘ ║
╚══════════════════════════════════════════╝
```

This requires storing the parsed text for context files in the app state so it
can be displayed — the `ProjectContext` already holds it.

#### 5c. Group display

When a group contains mixed file types, show both icons in the member list:

```
📎 D028_Requirements (3 files)
  ├ 📄 D028_SRS.docx
  ├ 📎 D028_Fields.xlsx
  └ 📎 D028_Workflow.vsdx
```

#### 5d. Progress bar / counters

Today the progress bar tracks `completed / total` files.  Adjust so that
context-only files are **excluded** from the denominator (they complete
instantly during parsing and don't go through the LLM pipeline):

```rust
let total_primary = files.iter()
    .filter(|p| FileRole::from_extension(ext_of(p)) == FileRole::Primary)
    .count();
```

#### 5e. Tooltip / legend

Add a small legend line near the file list header:

```
📄 = generates Gherkin    📎 = reference context
```

### 6. Group Logic Edge Cases

| Scenario | Behaviour |
|----------|-----------|
| Group with only Word files | Process as today → GroupResult |
| Group with Word + Excel/Visio | Process as today (all content merged) → GroupResult |
| Group with only Excel/Visio | Skip — emit status "ℹ Group X has no primary documents" |
| Ungrouped Excel/Visio | Parsed → context only, no LLM call |
| Ungrouped Word | Processed as today → FileResult |
| Zero Word files in entire batch | Emit warning "⚠ No Word documents found — nothing to transform" |

### 7. Cache Impact

The LLM cache key currently includes the file's own content + context hash.
Since context files now contribute to the prompt of every primary file, the
context hash will change whenever a context file is added/removed.  This is
**correct behaviour** — a different set of reference data should invalidate
stale caches.

No cache schema changes needed; the existing composite hash naturally
incorporates the changed prompts.

### 8. RAG Impact

Context files should still be indexed in the RAG vector store.  When a primary
file's prompt triggers `dynamic_context`, relevant chunks from Excel/Visio
files can surface automatically.  No changes needed — `add_file()` on
`ProjectContext` already stores all parsed content.

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/parser/mod.rs` | Add `FileRole` enum, extend `ParseResult`, set role in `parse_file()` |
| `src/context.rs` | Add `role` field to `FileContent`, add `build_context_only_summary()` |
| `src/app.rs` | Skip context files in work-item creation, update file-list rendering, add context-file info panel, adjust progress counters |
| `src/llm/mod.rs` | Inject context-only summary into `generate()` / `generate_group()` chat history |

## Files Unchanged

| File | Reason |
|------|--------|
| `src/parser/word.rs` | Word parser stays the same |
| `src/parser/excel.rs` | Excel parser stays the same (still parsed, just not transformed) |
| `src/parser/visio.rs` | Visio parser stays the same |
| `src/rag.rs` | Context files still get indexed |
| `src/memory.rs` | Factoid extraction operates on combined Gherkin — only primary outputs |
| `src/gherkin.rs` | No changes |
| `src/session.rs` | No changes |
| `src/openspec.rs` | OpenSpec export operates on Gherkin results — only primary outputs |

---

## Estimated Scope

~200–300 lines changed across 4 files.  No new dependencies.  Fully
backward-compatible with existing cached results (cache misses only, no
corruption).
