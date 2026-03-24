# Feature: Validation Source-of-Truth (Golden Gherkin / Markdown)

## Status: Planned

## Summary

Add a dedicated **"Add Validation Files"** workflow that lets users import hand-crafted
(or previously approved) `.feature` and `.md` files as a **source of truth**. When
generation runs, the pipeline compares generated output against validation files,
extracts systematic patterns of difference, and uses those patterns to self-correct
the final output — closing the gap between what the LLM produces and what the team
actually expects.

---

## Motivation

- Gherkin output quality varies across documents and LLM models. Human reviewers
  repeatedly fix the same categories of mistakes (wrong lifecycle phase, invented
  fields, incorrect cardinality).
- Today the only feedback loop is manual refinement one file at a time.
- By providing a set of approved "golden" files, the system can **learn the team's
  conventions** at generation time and apply them automatically to every file in the
  batch — without fine-tuning or RLHF.

---

## Terminology

| Term | Definition |
|------|-----------|
| **Validation file** | A `.feature` or `.md` file provided by the user as the authoritative reference |
| **Golden set** | The collection of all validation files for the current session |
| **Generated artifact** | The Gherkin or Markdown output produced by the LLM pipeline |
| **Diff pattern** | A recurring, categorised difference between generated vs golden output |
| **Correction prompt** | An LLM prompt that describes the extracted patterns and asks for a revised artifact |

---

## High-Level Flow

```
┌─────────────┐     ┌───────────────────┐     ┌──────────────────────┐
│  User adds   │     │  User adds input  │     │  User clicks         │
│  validation  │     │  documents (.docx  │     │  "Generate"          │
│  files       │     │  .xlsx .vsdx)     │     │                      │
└──────┬───────┘     └────────┬──────────┘     └──────────┬───────────┘
       │                      │                           │
       ▼                      ▼                           ▼
  ┌─────────┐          ┌───────────┐             ┌────────────────┐
  │ Parse & │          │  Parse    │             │ Phase 2: LLM   │
  │ store   │          │  as usual │             │ generate (as   │
  │ golden  │          │           │             │ today)         │
  │ set     │          │           │             │                │
  └────┬────┘          └───────────┘             └───────┬────────┘
       │                                                 │
       │              ┌──────────────────────────────────┘
       │              ▼
       │     ┌──────────────────┐
       │     │ Phase 2.5: Match │
       │     │ generated ↔      │
       └────▶│ golden by name   │
             │ or content       │
             └───────┬──────────┘
                     │
                     ▼
          ┌─────────────────────┐
          │ Phase 2.6: Diff &   │
          │ extract patterns    │
          │ across ALL pairs    │
          └───────┬─────────────┘
                  │
                  ▼
        ┌──────────────────────┐
        │ Phase 2.7: Correction│
        │ LLM pass — apply     │
        │ patterns to fix EACH │
        │ generated artifact   │
        └───────┬──────────────┘
                │
                ▼
         ┌──────────────┐
         │ Final output  │
         │ (corrected)   │
         └──────────────┘
```

---

## Phase 0 — Data Model & Storage

### 0.1 New types

```
// src/app.rs  (or new src/validation.rs)

/// A parsed validation file ready for comparison.
struct ValidationFile {
    /// Original file path on disk
    path: PathBuf,
    /// Stem used for matching (e.g. "D028_Req")
    match_key: String,
    /// The format
    kind: ValidationKind,
    /// Parsed Gherkin document (if .feature)
    gherkin: Option<GherkinDocument>,
    /// Parsed Markdown document (if .md)
    markdown: Option<MarkdownDocument>,
    /// Raw text content (for fallback diffing)
    raw_text: String,
}

enum ValidationKind {
    Gherkin,   // .feature files
    Markdown,  // .md files
}
```

### 0.2 App state additions

```rust
// In DockOckApp:

/// Validation golden set — source-of-truth files added by the user
validation_files: Vec<ValidationFile>,
```

### 0.3 Session persistence

Add `validation_paths: Vec<PathBuf>` to `SessionData`. On restore, re-parse each
path (files may have changed on disk between sessions).

---

## Phase 1 — UI: Import Validation Files

### 1.1 New button: "📋 Add Validation Files"

Location: Left panel toolbar, after "📁 Add Folder" and before "🗑 Clear".

Behaviour:
- Opens file dialog filtered to `*.feature` and `*.md` extensions
- Parses each selected file immediately:
  - `.feature` → `GherkinDocument::parse_from_llm_output(raw, filename)`
  - `.md` → `MarkdownDocument::parse_from_llm_output(raw, filename)`
- Computes `match_key` from stem: `"D028_Req.feature"` → `"D028_Req"`
- De-duplicates against existing `validation_files`
- Status: `"📋 Added N validation file(s)"`

### 1.2 New button: "📋 Add Validation Folder"

Same as above but recursive folder walk (reuse `collect_supported_files` logic,
new filter for `.feature` / `.md` extensions).

### 1.3 Validation file list in UI

Below the main file list (or in a collapsible section):

```
──── Validation Files (Source of Truth) ────
  📋 D028_Req.feature  (3 scenarios)
  📋 D029_Onboarding.feature  (5 scenarios)
  📋 Architecture.md  (8 sections)
  [🗑 Clear Validation]
```

Each item shows:
- File name
- Brief stats (scenario count / section count)
- Click to preview in right panel (read-only rendered view)

### 1.4 Validation mode indicator

When validation files are present, show a badge/indicator next to the Generate
button: `"⚡ Generate (with validation)"` or a subtle `"📋 N golden files loaded"`
status bar message. This signals the pipeline will run the extra correction phase.

---

## Phase 2 — Gherkin Parser Enhancement (`.feature` file ingestion)

### 2.1 Parse `.feature` files

`GherkinDocument::parse_from_llm_output` already handles free-form Gherkin.
However we should add/verify support for:

- `Background:` blocks (shared Given steps)
- `@tags` on Feature and Scenario lines
- `Examples:` tables in Scenario Outlines (table rows with `|` delimiters)
- `"""` doc strings in steps
- Comments (`#` lines)

If the existing parser doesn't handle these, extend it. Validation files are
human-written and likely use the full Gherkin spec.

### 2.2 Canonical normalisation

For comparison purposes, produce a **normalised** form of each Gherkin document:

```rust
impl GherkinDocument {
    /// Returns a normalised representation suitable for structural comparison.
    /// - Scenarios sorted by title
    /// - Steps trimmed and lowercased for fuzzy matching
    /// - Tags collected separately
    fn to_normalised(&self) -> NormalisedGherkin { ... }
}
```

This ensures cosmetic differences (ordering, whitespace, casing) don't dominate
the diff.

---

## Phase 3 — Matching: Generated ↔ Validation

### 3.1 Match strategy

Each generated artifact needs to find its corresponding validation file (if one
exists). Matching is done in priority order:

1. **Exact stem match**: generated from `D028_Req.docx` matches `D028_Req.feature`
2. **Stem prefix/suffix**: `D028_Req_v2.feature` matches `D028_Req.docx`
3. **Feature title match**: compare `Feature:` title strings (Levenshtein distance < 30%)
4. **No match**: generated artifact has no golden reference → skip correction phase

Output: `Vec<(GeneratedArtifact, Option<ValidationFile>)>`

### 3.2 Unmatched validation files

Validation files with no corresponding input document should be reported in the
log: `"⚠ Validation file D099_Foo.feature has no matching input document"`.
These can still contribute to **pattern extraction** (Phase 4) by showing the
team's preferred style.

---

## Phase 4 — Diff & Pattern Extraction

This is the core intelligence of the feature. It runs **after** generation
completes and **before** the correction pass.

### 4.1 Structural diff (per-pair)

For each `(generated, golden)` pair:

```rust
struct PairDiff {
    source: String,                     // e.g. "D028_Req"
    missing_scenarios: Vec<String>,     // in golden but not generated
    extra_scenarios: Vec<String>,       // in generated but not golden
    scenario_diffs: Vec<ScenarioDiff>,  // matched scenarios with step-level diffs
    style_diffs: Vec<StyleDiff>,        // naming conventions, tag patterns, etc.
}

struct ScenarioDiff {
    title: String,
    missing_steps: Vec<Step>,
    extra_steps: Vec<Step>,
    modified_steps: Vec<(Step, Step)>,  // (generated, golden)
}

enum StyleDiff {
    TagConvention { expected: String, actual: String },
    NamingPattern { expected_pattern: String, example: String },
    StepPhrasing { category: String, expected: String, actual: String },
    StructuralPattern { description: String },
}
```

For Markdown pairs, an analogous `MarkdownPairDiff`:

```rust
struct MarkdownPairDiff {
    source: String,
    missing_sections: Vec<String>,
    extra_sections: Vec<String>,
    section_diffs: Vec<SectionDiff>,
    style_diffs: Vec<StyleDiff>,
}
```

### 4.2 Cross-pair pattern aggregation

After computing diffs for every pair, aggregate recurring patterns:

```rust
struct AggregatedPatterns {
    /// Patterns seen in 2+ diffs (with frequency count)
    recurring: Vec<(DiffPattern, usize)>,
    /// Style conventions observed in golden files but not generated
    conventions: Vec<String>,
    /// Common structural rules (e.g. "always include a Background block")
    structural_rules: Vec<String>,
}

enum DiffPattern {
    /// Generated includes steps/scenarios about X that golden doesn't
    InventedContent { category: String, examples: Vec<String> },
    /// Golden includes steps/scenarios about X that generated misses
    MissingContent { category: String, examples: Vec<String> },
    /// LLM uses "Foo" but golden uses "Bar" phrasing
    TerminologyMismatch { generated_term: String, golden_term: String },
    /// LLM puts X in lifecycle phase Y but golden puts it in phase Z
    LifecycleMisplacement { concept: String, generated_phase: String, golden_phase: String },
    /// LLM treats optional field as mandatory (or vice versa)
    OptionalityMismatch { field: String, generated: String, golden: String },
    /// Cardinality mismatch (e.g. "1..n" vs "0..n")
    CardinalityMismatch { field: String, generated: String, golden: String },
    /// Step keyword usage pattern (e.g. golden always uses "And" not "Given" for multiple preconds)
    KeywordUsage { description: String },
}
```

### 4.3 LLM-assisted pattern extraction

Pure structural diff catches syntax-level differences. For **semantic** pattern
extraction, send paired examples to the LLM:

```
You are a Gherkin quality analyst. Below are pairs of (GENERATED, GOLDEN) Gherkin
features for the same source document. The GOLDEN version is the approved
source of truth.

=== PAIR 1 ===
GENERATED:
[generated feature text]
GOLDEN:
[golden feature text]

=== PAIR 2 ===
...

Analyse ALL pairs and extract RECURRING PATTERNS of difference.
For each pattern, provide:
1. Category (Invented, Missing, Terminology, Lifecycle, Optionality, Cardinality,
   Structure, Style)
2. Description of what the generator consistently does wrong
3. Concrete correction rule (what should be done instead)
4. Confidence: HIGH (3+ examples) / MEDIUM (2 examples) / LOW (1 example)

Output as a numbered list of patterns. Focus on RECURRING issues, not one-off typos.
```

This produces a human-readable pattern summary that feeds into the correction prompt.

### 4.4 Pattern caching

Store extracted patterns in session data so re-runs don't need to re-extract
unless golden files change:

```rust
// In SessionData:
validation_patterns: Option<CachedPatterns>,
validation_files_hash: Option<String>,  // hash of golden file contents
```

---

## Phase 5 — Correction Pass (LLM)

### 5.1 Correction preamble

A new constant: `CORRECTION_PREAMBLE`

```
You are a Gherkin quality improver. You have been given:
1. A GENERATED Gherkin feature file
2. A set of CORRECTION PATTERNS extracted from comparing generated files against
   the team's approved source-of-truth files

Your task: apply the correction patterns to improve the generated Gherkin.

RULES:
- Fix every instance of every pattern that applies to this file
- Do NOT remove scenarios or steps that are correct
- Do NOT add content that isn't in the source document
- Preserve the Feature title and overall structure
- Output ONLY the complete, corrected Gherkin feature file

=== CORRECTION PATTERNS ===
{patterns_block}

=== GENERATED GHERKIN ===
{generated_gherkin}

{optional: === GOLDEN REFERENCE (if exact match exists) ===
{golden_text}}
```

### 5.2 When golden match exists

If a validation file matched this specific generated artifact:
- Include the golden file as an additional reference section
- Add: `"If the golden reference covers the same Feature, prefer its structure,
  wording, and scenario organisation over the generated version — but ensure all
  documented requirements are still covered."`

### 5.3 When no golden match exists

The correction still runs using the aggregated patterns block. This is the key
benefit: patterns extracted from a few golden files improve **all** generated
artifacts, not just the ones with direct matches.

### 5.4 Pipeline integration

The correction pass runs as an additional LLM call **after** the existing review
step (in Standard/Full modes) or **after** generation (in Fast mode):

```
Fast mode:     Generate → Correct
Standard mode: Generate → Review → Correct
Full mode:     Extract → Generate → Review → Correct
```

This ensures the reviewer's syntax fixes are already applied before
pattern-based correction.

### 5.5 Markdown correction

An analogous `MARKDOWN_CORRECTION_PREAMBLE` for Markdown mode, focusing on:
- Section structure and ordering
- Completeness of coverage
- Cross-reference conventions
- Diagram and schema formatting

---

## Phase 6 — UI: Diff Visualisation & Pattern Review

### 6.1 Pattern summary panel

When validation files are loaded and generation completes, show a new panel
(or tab) displaying the extracted patterns:

```
──── Extracted Patterns (from 4 golden pairs) ────

1. 🔴 HIGH — Invented Content
   Generator adds "System validates field format" steps that don't appear
   in the source documents.
   → Correction: Remove validation steps not explicitly documented.

2. 🟡 MEDIUM — Lifecycle Misplacement
   Generator places "edit" scenarios under [Creation] phase; golden files
   put them under [Maintenance].
   → Correction: Move edit/update scenarios to [Maintenance] phase.

3. 🟡 MEDIUM — Terminology Mismatch
   Generator uses "the user" but golden files use "the Operator".
   → Correction: Use entity names from the project glossary.
```

Users can:
- Toggle patterns on/off (exclude low-confidence ones)
- Edit correction rules (fine-tune before re-running)
- Save patterns for future sessions

### 6.2 Before/After diff view

For files that have both a golden match and a corrected output, show a
three-way comparison:

```
[Generated (raw)]  |  [Golden (truth)]  |  [Corrected (final)]
```

Use the existing `diff_gherkin()` from session.rs (LCS-based diff) to
highlight changes.

### 6.3 Validation score

Show a per-file "alignment score" indicating how close the final output
is to the golden reference:

```
D028_Req.feature  — 87% aligned with golden  (was 62% before correction)
```

Score = 1 − (edit_distance / max(len_generated, len_golden)), computed on
normalised forms.

---

## Phase 7 — Extended Indexing Integration

### 7.1 Index validation patterns

Store extracted patterns in the `validation_patterns` MongoDB collection
so they can be retrieved by the RAG Chat module:

```rust
pub const VALIDATION_PATTERNS: CollectionConfig = CollectionConfig {
    name: "validation_patterns",
    index_name: "validation_patterns_vector_index",
};
```

### 7.2 Chat awareness

When users ask questions via Chat, include relevant validation patterns
in context: *"The team's golden files show that edit scenarios should be
in the Maintenance phase, not Creation."*

### 7.3 MCP tool

Expose validation patterns via MCP as a new tool:
`get_validation_patterns` — returns the aggregated pattern list for
the current session.

---

## Phase 8 — DependencyGraph Mode Support

The same pattern applies to DependencyGraph output mode:

- **Validation files**: JSON files matching the `DependencyGraph` schema
- **Diff**: Compare entities, relationships, business rules
- **Patterns**: Missing entity types, relationship mismatches, rule categorisation errors
- **Correction preamble**: `DEPGRAPH_CORRECTION_PREAMBLE`

This is lower priority than Gherkin/Markdown but uses the same infrastructure.

---

## Implementation Order

| Step | Files Modified | Estimated Effort |
|------|---------------|-----------------|
| 0. Data model + `ValidationFile` struct | `src/app.rs` (or new `src/validation.rs`) | Small |
| 1. UI: Add Validation Files / Folder buttons | `src/app.rs` | Small |
| 2. `.feature` parser enhancement | `src/gherkin.rs` | Medium |
| 3. Matching logic (stem + title) | `src/validation.rs` | Small |
| 4a. Structural diff engine | `src/validation.rs` | Medium |
| 4b. LLM pattern extraction prompt | `src/llm/mod.rs` | Medium |
| 4c. Pattern aggregation | `src/validation.rs` | Medium |
| 5a. `CORRECTION_PREAMBLE` + LLM call | `src/llm/mod.rs` | Medium |
| 5b. Pipeline integration (all 3 modes) | `src/llm/mod.rs` | Medium |
| 6a. Pattern summary panel UI | `src/app.rs` | Medium |
| 6b. Three-way diff view | `src/app.rs`, `src/session.rs` | Medium |
| 6c. Alignment score | `src/validation.rs` | Small |
| 7. MongoDB indexing + Chat/MCP | `src/rag.rs`, `src/chat.rs`, `src/mcp.rs` | Medium |
| 8. Session persistence | `src/session.rs` | Small |

**Recommended implementation sequence:**
Phases 0 → 1 → 2 → 3 → 4a → 4b → 5a → 5b → 4c → 6a → 6b → 6c → 7 → 8

---

## Files Created / Modified

| File | Action |
|------|--------|
| `src/validation.rs` | **NEW** — ValidationFile, matching, diffing, pattern aggregation |
| `src/app.rs` | Modify — new UI buttons, validation panel, DockOckApp fields, pipeline wiring |
| `src/gherkin.rs` | Modify — enhanced parser for full Gherkin spec, normalisation fn |
| `src/markdown.rs` | Modify — normalisation fn for Markdown comparison |
| `src/llm/mod.rs` | Modify — CORRECTION_PREAMBLE, correction LLM call, pipeline integration |
| `src/session.rs` | Modify — persist validation paths + cached patterns |
| `src/rag.rs` | Modify — new collection for validation patterns |
| `src/chat.rs` | Modify — include patterns in chat context |
| `src/mcp.rs` | Modify — new `get_validation_patterns` tool |
| `src/parser/mod.rs` | Modify — add `.feature` and `.md` to ACCEPTED_EXTENSIONS (separate constant) |
| `src/main.rs` | Modify — add `mod validation;` |

---

## Edge Cases & Decisions

1. **Mixed output modes**: If user has golden `.feature` files but runs in Markdown
   mode (or vice versa), skip the correction phase for mismatched types and log a
   warning.

2. **Large golden sets**: If the user adds 50+ validation files, the LLM pattern
   extraction prompt will be too large. Batch into groups of 8–10 pairs per
   extraction call, then merge pattern lists.

3. **No golden matches at all**: If no validation files match any input documents,
   still run pattern extraction on the golden files alone (style analysis) and
   apply those conventions to generated output.

4. **Conflicting patterns**: If pattern extraction finds contradictory rules
   (e.g. two golden files disagree), the LLM prompt should note the conflict and
   prefer the majority pattern.

5. **Performance**: The correction pass adds one LLM call per generated artifact.
   For large batches (20+ files), this is significant. Consider:
   - Applying correction only to files where diff score < threshold (e.g. < 80%)
   - Running correction calls concurrently (reuse existing TaskTracker + semaphore)

6. **Incremental updates**: If user adds more golden files mid-session, invalidate
   cached patterns and re-extract on next generation.

7. **Validation-only diffing**: Allow users to run diffing without re-generating.
   Button: `"📊 Compare Against Validation"` — runs Phase 3+4 on existing results.

---

## Future Extensions

- **Fine-grained pattern toggles**: Per-pattern enable/disable in the UI, persisted
  across sessions.
- **Pattern library**: Export/import pattern sets as JSON for sharing between teams.
- **Automatic golden promotion**: When a user rates a generated file 👍, offer to
  add it to the golden set.
- **Regression detection**: When re-generating after a model change, compare new
  output against both golden files AND previous output to detect regressions.
- **Weighted correction**: Weight patterns by frequency × confidence for priority
  ordering in the correction prompt.
