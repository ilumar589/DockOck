# Feature: File Groups & Shared Context

## Overview

Files that are logically related (e.g. `D028_Requirements.docx`, `D028_FlowChart.vsdx`,
`D028_DataModel.xlsx`) should be **grouped together** so their combined context is fed to
the LLM as a single, unified prompt — producing a single, richer Gherkin `.feature` file
per group instead of one `.feature` per input file.

Two mechanisms are provided:

1. **Auto-grouping** — files whose names share the same prefix *and* suffix
   (extension excluded) are automatically placed in the same group.
   Example: `D028_Requirements.docx` + `D028_Requirements.vsdx` → group `D028_Requirements`.
2. **Manual grouping (UI)** — the user can create, rename, and populate arbitrary groups
   via drag-and-drop or a group-management panel, overriding or supplementing auto-grouping.

Files that belong to no group are processed individually as today.

---

## Data Model Changes

### New type: `FileGroup` (`src/context.rs`)

```rust
/// A named collection of files whose content is merged into a single
/// aggregated context before Gherkin generation.
#[derive(Debug, Clone)]
pub struct FileGroup {
    /// Human-readable group label (e.g. "D028_Requirements")
    pub name: String,
    /// Ordered list of file paths that belong to this group
    pub members: Vec<PathBuf>,
}
```

### Extend `ProjectContext`

```rust
pub struct ProjectContext {
    pub file_contents: HashMap<String, FileContent>,
    pub entities: Vec<String>,
    pub notes: Vec<String>,
    // ── NEW ──
    /// Groups of related files whose context is merged before generation
    pub groups: Vec<FileGroup>,
}
```

Add a helper:

```rust
impl ProjectContext {
    /// Build a merged context string for every file in a group.
    /// Returns the concatenation of each member's raw_text (with headers),
    /// capped at a configurable limit.
    pub fn build_group_context(&self, group: &FileGroup) -> String { ... }
}
```

### Extend `DockOckApp` (`src/app.rs`)

```rust
pub struct DockOckApp {
    // ... existing fields ...

    /// File groups — owned by the UI, sent to the background pipeline
    file_groups: Vec<FileGroup>,
    /// Whether auto-grouping by prefix+suffix is enabled
    auto_group_enabled: bool,
    /// Mapping: file path → group index (None = ungrouped)
    file_group_map: HashMap<PathBuf, usize>,
}
```

---

## Auto-Grouping Algorithm

Located in a new helper `fn compute_auto_groups(files: &[PathBuf]) -> Vec<FileGroup>`.

```
for each file:
    stem = file_stem (no extension)            e.g. "D028_Requirements"
    prefix = longest leading alphanumeric+_    e.g. "D028"
    suffix = remaining part                    e.g. "_Requirements"
    group_key = (prefix, suffix)               → only group when ≥ 2 files share key

files with same group_key → one FileGroup named "{prefix}{suffix}"
files with unique group_key → left ungrouped (processed individually)
```

To handle the "same prefix AND suffix" requirement as stated:

```
stem  = "D028_Requirements"
key   = stem                   // exact stem match across different extensions
group = files where stem matches
```

So `D028_Requirements.docx` + `D028_Requirements.vsdx` group because stems are identical.
`D028_Requirements.docx` + `D028_FlowChart.vsdx` do NOT auto-group (different stems).

The function lives in `src/context.rs` alongside `ProjectContext`.

---

## UI Changes (`src/app.rs`)

### Left Panel — File List & Group Management

The existing flat file list gains a **grouped view** mode:

```
┌── 📂 Files ──────────────────────────────┐
│  ☑ Auto-group by name                    │
│  [➕ Add Files]  [🗑 Clear]  [📎 New Group]│
│                                           │
│  ▼ Group: D028_Requirements  ✏  ✖        │
│    ├ D028_Requirements.docx   [x]         │
│    └ D028_Requirements.vsdx   [x]         │
│                                           │
│  ▼ Group: D029_Login  ✏  ✖               │
│    ├ D029_Login.docx          [x]         │
│    └ D029_Login.xlsx          [x]         │
│                                           │
│  ── Ungrouped ──                          │
│    D099_Standalone.docx                   │
└───────────────────────────────────────────┘
```

**Interactions:**

| Action | How |
|--------|-----|
| Toggle auto-grouping | Checkbox at top. Recomputes `file_groups` on change. |
| Create manual group | Click `📎 New Group` → prompted for name → empty group added. |
| Rename group | Click ✏ pencil icon → inline text edit. |
| Delete group | Click ✖ → members become ungrouped. |
| Add file to group | Right-click file → "Move to group…" submenu listing groups. Or drag-and-drop (egui drag). |
| Remove file from group | Click [x] button next to file in group. File becomes ungrouped. |
| Select group | Click group header → right panel shows **merged** Gherkin preview. |
| Select ungrouped file | Click file → right panel shows its individual Gherkin. |

When auto-group is on, auto-detected groups are **locked** (shown with a 🔒 icon and cannot
be manually edited), but the user can still create additional manual groups.

### Right Panel — Merged Gherkin Output

When a **group** is selected, the right panel shows the single merged `.feature` file
generated for that group. Save produces `{group_name}.feature`.

### Output Naming

| Input | Output file |
|-------|-------------|
| Ungrouped `D099_Standalone.docx` | `D099_Standalone.feature` |
| Group `D028_Requirements` (2 files) | `D028_Requirements.feature` |

---

## Pipeline Changes (`src/app.rs` → `process_files`, `src/llm/mod.rs`)

### Phase 1 — Parsing (unchanged)

All files are still parsed individually in parallel. Each file's `ParseResult` is stored
in `ProjectContext.file_contents` as today.

### Phase 1.5 — Context Aggregation (NEW)

After parsing, build **merged work items**:

```rust
enum WorkItem {
    /// A single ungrouped file
    Single {
        path: PathBuf,
        file_type: String,
        text: String,
        images: Vec<ExtractedImage>,
    },
    /// A group of related files whose content is merged
    Group {
        group_name: String,
        members: Vec<(PathBuf, String, String, Vec<ExtractedImage>)>,
        merged_text: String,
        merged_images: Vec<ExtractedImage>,
    },
}
```

For each `Group`, build `merged_text` by concatenating member texts with separators:

```
=== Document 1: D028_Requirements.docx (Word) ===
<raw_text of docx>

=== Document 2: D028_Requirements.vsdx (Visio) ===
<raw_text of vsdx>
```

Images from all members are combined into `merged_images`.

### Phase 2 — LLM Pipeline

The existing `process_file` call remains for `WorkItem::Single`.

For `WorkItem::Group`, call a new variant:

```rust
impl AgentOrchestrator {
    pub async fn process_group(
        &self,
        group_name: &str,
        merged_text: &str,
        images: &[ExtractedImage],
        context: &ProjectContext,
        status_tx: &Sender<String>,
    ) -> Result<String> { ... }
}
```

This method follows the same Extract → Generate → Review pipeline but:

- The **extractor** prompt includes: *"The following content comes from multiple related
  documents that describe the same feature/process. Produce a single unified structured
  summary."*
- The **generator** prompt includes: *"The following structured summary was synthesised
  from multiple related documents. Generate a single cohesive Gherkin Feature file that
  covers all scenarios described across the documents."*
- The cross-file context (`build_summary`) **excludes** the group's own members (they're
  already in the merged text) but still includes other files/groups for cross-references.

### Result Handling

`ProcessingEvent::FileResult` is extended (or a new variant added):

```rust
pub enum ProcessingEvent {
    // ... existing ...
    /// A group of files has been fully processed
    GroupResult {
        group_name: String,
        member_paths: Vec<PathBuf>,
        gherkin: GherkinDocument,
        elapsed: std::time::Duration,
    },
}
```

Results are stored in a new map in `DockOckApp`:

```rust
group_results: HashMap<String, GherkinDocument>,
```

---

## `build_summary` Awareness of Groups

Update `ProjectContext::build_summary()` to accept an optional exclusion set:

```rust
pub fn build_summary_excluding(&self, exclude: &HashSet<String>) -> String {
    // Same as build_summary() but skips files whose path is in `exclude`
}
```

When generating for group `D028_Requirements`, the exclusion set contains the paths of
`D028_Requirements.docx` and `D028_Requirements.vsdx`. This prevents redundant context
injection (the files are already in the merged prompt).

---

## LLM Prompt Adjustments (`src/llm/mod.rs`)

### New Preambles

Add `GROUP_EXTRACTOR_PREAMBLE` and `GROUP_GENERATOR_PREAMBLE` that instruct the LLM about
multi-document input:

```rust
const GROUP_EXTRACTOR_PREAMBLE: &str = "\
You are a business analyst. You will receive content extracted from \
MULTIPLE related documents that describe the same system or process. \
Your task is to produce a single unified structured summary that \
synthesises the information from all documents, resolving any overlaps \
or contradictions. Identify all actors, systems, data fields, business \
rules, and process flows.";

const GROUP_GENERATOR_PREAMBLE: &str = "\
You are a Gherkin expert. You will receive a structured summary \
synthesised from multiple related documents. Generate a single, \
cohesive Gherkin Feature file. Avoid duplicate scenarios. When \
different documents describe the same process from different angles, \
merge them into comprehensive scenarios.";
```

---

## Affected Files Summary

| File | Changes |
|------|---------|
| `src/context.rs` | Add `FileGroup`, `build_group_context()`, `build_summary_excluding()`, `compute_auto_groups()` |
| `src/app.rs` | Add `file_groups`, `auto_group_enabled`, `file_group_map`, `group_results` fields. New UI for group management in left panel. Extend `ProcessingEvent` with `GroupResult`. Update `process_files()` to build `WorkItem`s and dispatch groups. |
| `src/llm/mod.rs` | Add `process_group()` method, `GROUP_EXTRACTOR_PREAMBLE`, `GROUP_GENERATOR_PREAMBLE`. |
| `src/gherkin.rs` | No changes needed (GherkinDocument is generic enough). |
| `src/parser/*` | No changes needed (parsing stays per-file). |

---

## Implementation Order

1. **`src/context.rs`** — Add `FileGroup`, `compute_auto_groups()`,
   `build_group_context()`, `build_summary_excluding()`.
2. **`src/llm/mod.rs`** — Add group preambles and `process_group()`.
3. **`src/app.rs` (data)** — Add new fields, extend `ProcessingEvent`, update
   `process_files()` to handle groups.
4. **`src/app.rs` (UI)** — Build the grouped file list, auto-group toggle,
   manual group management, and merged result display.
5. **Testing** — Add unit tests for `compute_auto_groups()`, `build_group_context()`,
   and `build_summary_excluding()`. Add an integration example in `examples/`.

---

## Edge Cases

| Case | Behaviour |
|------|-----------|
| Single file in group | Treated as a regular single file (no merge overhead). |
| File added after groups computed | Auto-groups recomputed; manual groups unaffected. |
| File removed from group | If last member removed, group is deleted. |
| Same file in two manual groups | Not allowed — a file belongs to at most one group. |
| Very large merged text | `MAX_INPUT_CHARS` limit applied to the merged text. Each member gets a proportional share: `MAX_INPUT_CHARS / num_members`. |
| Group with mixed file types | Fully supported — Word + Visio + Excel merged text includes type headers. |
| Empty group | Ignored during processing. |
