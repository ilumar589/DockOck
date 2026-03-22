# Feature: Markdown Knowledge Base Output Mode

## Overview

Add a third `OutputMode::Markdown` that generates rich, structured Markdown files from input documents instead of Gherkin `.feature` files. These Markdown files serve as a **comprehensive knowledge base** — capturing database schemas, data models, architecture diagrams, entity relationships, cross-document references, Visio flow data, and Excel test data — formatted so that another AI agent can consume them to implement features end-to-end.

The Markdown output is **tech-stack-aware**: users select a target technology stack from a config file (`tech_stacks.json`) via the UI. The selected stack (e.g., .NET 10 + React + PostgreSQL + Redis + Keycloak + Docker) is injected into every LLM prompt so that database schemas use the correct SQL dialect, data models use the right type system, architecture diagrams reference the actual frameworks, and API contracts match the chosen protocols.

---

## Implementation Plan — 10 Steps

| Step | Title | Files Touched | Depends On |
|------|-------|---------------|------------|
| 1 | Add `OutputMode::Markdown` enum variant | `src/llm/mod.rs` | — |
| 2 | Create `MarkdownDocument` data model | `src/markdown.rs` (new) | Step 1 |
| 3 | Write dedicated LLM prompt templates | `src/llm/mod.rs` | Step 2 |
| 4 | Implement `MarkdownDocument` rendering | `src/markdown.rs` | Step 2 |
| 5 | Wire the pipeline — orchestrator branching | `src/llm/mod.rs`, `src/app.rs` | Steps 1–4 |
| 6 | Add UI controls and display panel | `src/app.rs` | Step 5 |
| 7 | Integrate session persistence and caching | `src/session.rs`, `src/cache.rs` | Steps 2, 5 |
| 8 | Cross-document relationship graph in Markdown | `src/markdown.rs`, `src/depgraph.rs` | Steps 2, 5 |
| 9 | Tech stack configuration and prompt injection | `src/tech_stack.rs` (new), `tech_stacks.json` (new), `src/llm/mod.rs`, `src/app.rs`, `src/session.rs` | Steps 3, 5 |
| 10 | End-to-end tests and example | `examples/test_markdown.rs` (new) | Steps 1–9 |

---

## Step 1 — Add `OutputMode::Markdown` Enum Variant

**Goal:** Extend the existing `OutputMode` enum so the rest of the codebase can branch on the new mode.

**Files:** `src/llm/mod.rs`

**What to do:**

1. Open `src/llm/mod.rs` and find the `OutputMode` enum (currently at line ~150):
   ```rust
   pub enum OutputMode {
       Gherkin,
       DependencyGraph,
   }
   ```
2. Add a third variant:
   ```rust
   pub enum OutputMode {
       Gherkin,
       DependencyGraph,
       Markdown,
   }
   ```
3. Update the `Display` impl (currently at line ~165):
   ```rust
   Self::Markdown => write!(f, "Markdown (.md)"),
   ```
4. Update the `ALL` array (currently at line ~175):
   ```rust
   pub const ALL: [OutputMode; 3] = [Self::Gherkin, Self::DependencyGraph, Self::Markdown];
   ```
5. After this change, run `cargo check`. The compiler will emit "non-exhaustive pattern" errors everywhere `OutputMode` is matched. **Do not fix those yet** — they will guide Steps 5–7.

**Prompt for implementing agent:**
> You are modifying the DockOck Rust project. In `src/llm/mod.rs`, the `OutputMode` enum at line ~150 has two variants: `Gherkin` and `DependencyGraph`. Add a third variant `Markdown`. Update the `Display` impl to print `"Markdown (.md)"` for it. Update the `ALL` const array to include all three variants (change the array size from 2 to 3). Do NOT fix any downstream match-exhaustiveness errors — those will be addressed in later steps. Run `cargo check` to confirm the enum change compiles, and list all resulting non-exhaustive-match errors with their file and line numbers.

---

## Step 2 — Create `MarkdownDocument` Data Model

**Goal:** Define a structured AST for rich Markdown knowledge-base documents, analogous to `GherkinDocument` (in `src/gherkin.rs`) and `DependencyGraph` (in `src/depgraph.rs`).

**Files:** `src/markdown.rs` (new file), `src/main.rs` (add `mod markdown;`)

**Data structures to define:**

```rust
// src/markdown.rs

use serde::{Serialize, Deserialize};

/// A comprehensive knowledge-base document generated from source files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownDocument {
    /// Document title derived from the source file name or Feature name.
    pub title: String,
    /// One-paragraph executive summary of what this document covers.
    pub summary: String,
    /// The source file(s) this was generated from.
    pub source_files: Vec<String>,
    /// Ordered sections of the document.
    pub sections: Vec<Section>,
    /// Cross-references to other documents in the project.
    pub cross_references: Vec<CrossReference>,
}

/// A top-level section within the Markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    /// Section heading text.
    pub heading: String,
    /// The kind of content this section holds.
    pub kind: SectionKind,
    /// Raw markdown body of the section.
    pub body: String,
    /// Optional sub-sections (for nested headings).
    pub subsections: Vec<Section>,
}

/// Categorises sections so consumers know what data to expect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SectionKind {
    /// Free-form narrative (business rules, process descriptions).
    Narrative,
    /// Database schema: tables, columns, types, constraints.
    DatabaseSchema,
    /// Data model: entities, attributes, relationships.
    DataModel,
    /// Architecture diagram description (from Visio or images).
    ArchitectureDiagram,
    /// Entity-relationship description.
    EntityRelationship,
    /// State machine / lifecycle.
    StateMachine,
    /// Excel-sourced test data or reference tables.
    TestData,
    /// API contracts or service interfaces.
    ApiContract,
    /// Business rules and validation logic.
    BusinessRules,
    /// UI/UX wireframe description.
    UiDescription,
    /// Extracted image/diagram content.
    ImageContent,
}

/// A reference from this document to another project document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossReference {
    /// Name of the referenced document (file name or logical name).
    pub target_document: String,
    /// Nature of the relationship.
    pub relationship: String,
    /// Brief description of what is shared or depended upon.
    pub description: String,
}
```

**What to do:**

1. Create `src/markdown.rs` with the structs above.
2. In `src/main.rs`, add `mod markdown;` alongside the existing `mod gherkin;` and `mod depgraph;`.
3. Run `cargo check` to confirm it compiles.

**Prompt for implementing agent:**
> You are adding a new module to the DockOck Rust project. Create a file `src/markdown.rs` that defines the `MarkdownDocument` AST — a structured representation of a rich knowledge-base Markdown file. The model must closely mirror the project's existing patterns: look at `src/gherkin.rs` (`GherkinDocument`, `Scenario`, `Step`) and `src/depgraph.rs` (`DependencyGraph`, `GraphNode`, `GraphEdge`) for conventions — use `#[derive(Debug, Clone, Serialize, Deserialize)]` on all types, keep field names snake_case. The structs needed are: `MarkdownDocument` (title, summary, source_files: Vec<String>, sections: Vec<Section>, cross_references: Vec<CrossReference>), `Section` (heading, kind: SectionKind, body: String, subsections: Vec<Section>), `SectionKind` enum (Narrative, DatabaseSchema, DataModel, ArchitectureDiagram, EntityRelationship, StateMachine, TestData, ApiContract, BusinessRules, UiDescription, ImageContent), and `CrossReference` (target_document, relationship, description). Then register the module in `src/main.rs` by adding `mod markdown;` next to the existing module declarations. Run `cargo check`.

---

## Step 3 — Write Dedicated LLM Prompt Templates

**Goal:** Create entirely new prompt preambles for the Markdown output mode. These must be *completely different* from the Gherkin prompts — not reworded versions, but prompts purpose-built for knowledge-base extraction.

**Files:** `src/llm/mod.rs`

**New constants to add** (place them after the existing `DEPGRAPH_*` constants, around line ~600):

### 3a. Markdown Extractor Preamble

```rust
pub const MARKDOWN_EXTRACTOR_PREAMBLE: &str = r#"You are a senior technical documentation architect.
Your task is to consume raw document content and decompose it into a richly structured knowledge inventory.

OUTPUT FORMAT — You must produce these sections in order. If a section has no relevant data, write "N/A".

## DATABASE SCHEMA
For every table, entity, or data store mentioned:
- Table/collection name
- Column/field list with: name, type, nullable?, default, constraints (PK, FK, unique, check)
- Index definitions if mentioned
- Relationships to other tables (FK references, junction tables)

## DATA MODELS
For every business object, DTO, or aggregate:
- Class/struct name and purpose
- Fields with types and validation rules
- Inheritance or composition hierarchies
- Serialization notes (JSON field names, XML element mappings)

## ARCHITECTURE
For every system component, service, or layer mentioned:
- Component name and responsibility
- Technology stack (language, framework, database)
- Communication protocols (REST, gRPC, message queue, event bus)
- Deployment topology (container, serverless, on-prem)
- ASCII or Mermaid representation of component interactions

## ENTITY RELATIONSHIPS
- Entity pairs and cardinality (1:1, 1:N, M:N)
- Relationship semantics (owns, references, depends-on, triggers)
- Lifecycle coupling (cascade delete, orphan rules)

## STATE MACHINES
For every entity with lifecycle states:
- State names and descriptions
- Transitions: from → to, trigger event, guard conditions
- Terminal states and error states

## BUSINESS RULES
- Rule ID and plain-language description
- Trigger (what event invokes it)
- Preconditions and postconditions
- Validation formula or pseudo-code if available

## PROCESS FLOWS
For every workflow or business process:
- Step sequence (numbered)
- Decision points with branch conditions
- Parallel paths
- Exception/error paths
- Actors involved at each step

## TEST DATA TABLES
For every table of test data, acceptance criteria, or example values:
- Reproduce the table in pipe-delimited Markdown format
- Preserve column headers exactly
- Note which scenarios each row is intended to validate

## UI / SCREENS
For every UI dialog, form, or page:
- Screen name and purpose
- Field list with control type, mandatory/optional, validation
- Button actions and navigation targets
- Layout notes (tabs, sections, groups)

## IMAGE / DIAGRAM CONTENT
For every embedded image or diagram:
- Diagram type (flowchart, ER, deployment, sequence, etc.)
- Full transcription of all text, labels, connectors
- Structure in Mermaid syntax when possible

## CROSS-DOCUMENT REFERENCES
- List every reference to another document (by name, ID, or hyperlink)
- Describe what is referenced and why

Keep all information faithful to the source. Do not invent data. Transcribe technical details verbatim.
Maximum output: 2000 words."#;
```

### 3b. Markdown Generator Preamble

```rust
pub const MARKDOWN_GENERATOR_PREAMBLE: &str = r#"You are a technical knowledge-base author.
Your task is to convert a structured knowledge inventory into a polished, comprehensive Markdown document
that another AI agent can consume to implement the described system feature-by-feature.

DOCUMENT STRUCTURE — Generate exactly this layout:

# <Feature/Document Title>

## Summary
One paragraph: what this document describes, which system area it covers, and key entities involved.

## Database Schema
For each table: render as a Markdown table with columns: Field | Type | Nullable | Default | Constraints.
Below each table, list foreign-key relationships as bullet points.
Include CREATE TABLE pseudo-SQL if the source contains enough detail.

## Data Models
For each entity/class:
- Heading with entity name
- Bullet list of fields with type annotations
- Validation rules as nested bullets
- Relationships to other models

## Architecture
- Component diagram in Mermaid ```mermaid fenced block
- Bullet list of each component with: name, responsibility, tech stack, protocol
- Data flow arrows described textually

## Entity Relationships
- ER diagram in Mermaid ```mermaid fenced block
- Prose description of each relationship with cardinality

## State Machines
- State diagram in Mermaid ```mermaid fenced block (stateDiagram-v2)
- Transition table: | From | To | Trigger | Guard |

## Business Rules
Numbered list. Each rule:
> **BR-NNN**: <description>
> - Trigger: <event>
> - Precondition: <condition>
> - Postcondition: <result>
> - Validation: <formula or pseudo-code>

## Process Flows
For each process:
- Flowchart in Mermaid ```mermaid fenced block
- Numbered step list with actor, action, decision branches

## Test Data
Reproduce every data table from the source as a Markdown pipe table.
Add a "Purpose" column if not present, explaining what each row verifies.

## UI Specifications
For each screen/dialog:
- Screen name heading
- Field table: | Field | Control | Required | Validation | Notes |
- Button list with actions
- Navigation flows

## Visio Diagram Content
For each Visio page:
- Page title
- All shapes and connections transcribed
- Flow direction and decision logic
- Mermaid equivalent where feasible

## Excel Reference Data
For each worksheet:
- Sheet name heading
- Full data table in Markdown pipe format
- Column type annotations
- Notable patterns or groupings

## Cross-References
Bullet list: each entry is `[Document Name](relative-link) — relationship description`.

## Appendix: Raw Extracted Content
Preserve a collapsed <details> block with the original extracted text for traceability.

OUTPUT RULES:
1. Output ONLY valid Markdown. No conversational text outside the document structure.
2. Mermaid diagrams must be syntactically valid.
3. Every piece of information from the source must appear — nothing omitted.
4. Tables must have header rows and separator rows.
5. Use heading levels consistently: # for title, ## for major sections, ### for subsections.
6. Cross-reference other project documents by name whenever the source mentions them.
7. End with a blank line."#;
```

### 3c. Markdown Reviewer Preamble

```rust
pub const MARKDOWN_REVIEWER_PREAMBLE: &str = r#"You are a technical documentation quality auditor.
Your task is to review a Markdown knowledge-base document and ensure it is complete, accurate, and
well-structured enough for another AI agent to implement the described system from it alone.

REVIEW CHECKLIST — verify each item and fix violations:

1. COMPLETENESS: Every section from the template must be present (even if "N/A").
   Cross-check against the source material — flag any omitted data.
2. DATABASE ACCURACY: Table schemas must have all columns with correct types.
   Foreign keys must reference existing tables. Constraints must match the source.
3. MERMAID VALIDITY: Every ```mermaid block must parse without errors.
   Common fixes: quote labels with special chars, ensure arrow syntax (-->), close subgraphs.
4. TABLE FORMATTING: Every Markdown table must have a header row and |---|---| separator.
   Columns must align. No empty tables.
5. CROSS-REFERENCES: Every mentioned external document must appear in the Cross-References section.
   Links must use consistent naming.
6. BUSINESS RULE FIDELITY: Rules must match source wording exactly. No invented rules.
   Conditions must be verbatim, not paraphrased.
7. STATE MACHINE COVERAGE: All states and transitions from the source must be present.
   No orphan states (unreachable or dead-end without documentation).
8. TEST DATA PRESERVATION: All rows from source tables must be reproduced exactly.
   No summarization of tabular data.
9. HEADING HIERARCHY: # > ## > ### — no skipped levels.
10. NO CONVERSATIONAL PROSE: Remove any "Here is the document..." or "I hope this helps" text.

Output ONLY the corrected Markdown document — no review commentary."#;
```

**What to do:**

1. Add the three constants above to `src/llm/mod.rs`, after the existing `DEPGRAPH_*` constants.
2. Make them `pub` so they're accessible from `app.rs`.
3. Run `cargo check` to confirm they compile (they're just string constants, so this is trivial).

**Prompt for implementing agent:**
> You are adding new LLM prompt templates to the DockOck Rust project in `src/llm/mod.rs`. The project already has `EXTRACTOR_PREAMBLE`, `GENERATOR_PREAMBLE`, `REVIEWER_PREAMBLE` (for Gherkin) and `DEPGRAPH_GENERATOR_PREAMBLE` (for dependency graphs). You need to add three NEW public string constants: `MARKDOWN_EXTRACTOR_PREAMBLE`, `MARKDOWN_GENERATOR_PREAMBLE`, and `MARKDOWN_REVIEWER_PREAMBLE`. Place them after the existing `DEPGRAPH_*` constants (around line ~600). The exact content of each constant is provided below. These prompts are completely different from the Gherkin prompts — they focus on extracting and rendering database schemas, data models, architecture diagrams (Mermaid), entity relationships, state machines, business rules, process flows, test data tables, UI specs, Visio content, Excel data, and cross-document references into structured Markdown. Copy the prompt text exactly as given. Run `cargo check` after.
>
> [Paste the three constant strings from Step 3 above]

---

## Step 4 — Implement `MarkdownDocument` Rendering

**Goal:** Add methods to `MarkdownDocument` that render it to a valid Markdown string, and parse one from raw LLM output.

**Files:** `src/markdown.rs`

**Methods to implement:**

```rust
impl MarkdownDocument {
    /// Render the document to a Markdown string.
    pub fn to_markdown_string(&self) -> String { /* ... */ }

    /// Best-effort parse of raw LLM markdown output into a MarkdownDocument.
    pub fn parse_from_llm_output(raw: &str, source_file: &str) -> Self { /* ... */ }
}
```

**Rendering rules for `to_markdown_string()`:**
- Start with `# {title}\n\n`
- Print `## Summary\n\n{summary}\n\n`
- For each section: print `## {heading}\n\n{body}\n\n`
- For each subsection: print `### {heading}\n\n{body}\n\n`
- End with `## Cross-References\n\n` followed by bullet list of cross-refs
- Each cross-ref: `- **{target_document}** ({relationship}): {description}`

**Parsing rules for `parse_from_llm_output()`:**
- Split on lines starting with `# ` or `## ` to identify title and sections
- Title = first `# ` line
- Summary = body under `## Summary` if present, otherwise first paragraph
- Each `## ` heading becomes a `Section` with appropriate `SectionKind` mapped from heading text:
  - "Database Schema" → `DatabaseSchema`
  - "Data Model" → `DataModel`
  - "Architecture" → `ArchitectureDiagram`
  - "Entity Relationship" → `EntityRelationship`
  - "State Machine" → `StateMachine`
  - "Business Rule" → `BusinessRules`
  - "Test Data" → `TestData`
  - "Process Flow" → `Narrative`
  - "UI" → `UiDescription`
  - "Visio" or "Diagram" → `ImageContent`
  - "Excel" → `TestData`
  - "Cross-Reference" → handled separately
  - Anything else → `Narrative`
- Cross-references extracted from `## Cross-References` section bullet items
- `source_files` = `vec![source_file.to_string()]`

**Prompt for implementing agent:**
> You are implementing rendering and parsing for `MarkdownDocument` in `src/markdown.rs` of the DockOck Rust project. The struct is already defined (from Step 2). Add two methods: `to_markdown_string(&self) -> String` which renders the full Markdown text from the AST (title as `#`, summary as `## Summary`, each section as `## heading` with body, subsections as `###`, and a `## Cross-References` bullet list at the end), and `parse_from_llm_output(raw: &str, source_file: &str) -> Self` which does a best-effort parse of raw LLM-generated Markdown into the struct. The parser should split on heading lines (`#` / `##` / `###`), map heading text to `SectionKind` variants using keyword matching (e.g., "Database" → `DatabaseSchema`, "Architecture" → `ArchitectureDiagram`, etc.), extract cross-references from the `## Cross-References` section (parse `- **target** (relationship): description` patterns), and fall back to `SectionKind::Narrative` for unrecognized headings. Follow the same error-tolerance style as `gherkin.rs`'s `parse_from_llm_output` — never panic, always produce a valid struct. Run `cargo check`.

---

## Step 5 — Wire the Pipeline — Orchestrator Branching

**Goal:** Make the `AgentOrchestrator` in `src/llm/mod.rs` and the `process_files()` function in `src/app.rs` support the new `OutputMode::Markdown` path.

**Files:** `src/llm/mod.rs`, `src/app.rs`

**Changes in `src/llm/mod.rs`:**

1. **In `process_file()`** (around line ~1300): There is a match or series of conditionals on `output_mode`. Add a branch for `OutputMode::Markdown`:
   - **Extraction phase**: Use `MARKDOWN_EXTRACTOR_PREAMBLE` instead of `EXTRACTOR_PREAMBLE`. The extraction system message should be the markdown extractor preamble.
   - **Generation phase**: Use `MARKDOWN_GENERATOR_PREAMBLE` instead of `GENERATOR_PREAMBLE`. The generation system message should be the markdown generator preamble.
   - **Review phase**: Use `MARKDOWN_REVIEWER_PREAMBLE` instead of `REVIEWER_PREAMBLE`. The review system message should be the markdown reviewer preamble.
   - The generation call should inject **all** context — not just Primary file content but also Context-only files (Excel/Visio) as full content, not summaries. This is key: for Markdown mode, Excel and Visio data must be included in their entirety (subject to model context limits), not just as supplementary context.
   - Return the raw Markdown string (same as Gherkin mode returns raw Gherkin string).

2. **Add a dedicated method `generate_markdown()`** (optional, for clarity):
   ```rust
   async fn generate_markdown(
       &self,
       file_name: &str,
       summary: &str,
       context_summary: &str,
       glossary: &str,
       rag_context: &str,
       cancel_token: &CancellationToken,
   ) -> Result<String> { /* ... */ }
   ```
   This mirrors the existing `generate()` method but uses `MARKDOWN_GENERATOR_PREAMBLE` and structures the multi-turn chat to request Markdown output.

3. **In the preamble-selection logic** (around line ~2823 in `app.rs`):
   ```rust
   crate::llm::OutputMode::Markdown => crate::llm::MARKDOWN_GENERATOR_PREAMBLE,
   ```

**Changes in `src/app.rs`:**

1. **In `process_files()`** (the main async pipeline): Where results are dispatched after LLM returns, add a branch:
   ```rust
   crate::llm::OutputMode::Markdown => {
       let md_doc = crate::markdown::MarkdownDocument::parse_from_llm_output(&raw_output, &file_name);
       // Store in a new HashMap<String, MarkdownDocument> field on DockOckApp
       // Send ProcessingEvent::MarkdownResult { file_name, document: md_doc }
   }
   ```

2. **Add a `markdown_results: HashMap<String, MarkdownDocument>` field** to `DockOckApp`.

3. **Add `ProcessingEvent::MarkdownResult`** variant to the event enum.

4. **Fix all `match output_mode` exhaustiveness errors** by adding `OutputMode::Markdown => { ... }` arms.

**Prompt for implementing agent:**
> You are wiring the Markdown output mode into DockOck's pipeline. The codebase has `OutputMode::Markdown` (Step 1) and `MarkdownDocument` (Step 2) already. Now integrate them:
>
> **In `src/llm/mod.rs`**: Find the `process_file()` method (around line ~1300). Wherever the code branches on `output_mode` or selects preamble constants, add the `Markdown` arm using `MARKDOWN_EXTRACTOR_PREAMBLE`, `MARKDOWN_GENERATOR_PREAMBLE`, and `MARKDOWN_REVIEWER_PREAMBLE`. Key difference from Gherkin mode: in Markdown mode, inject the FULL content of Context-only files (Excel, Visio) into the generation prompt — not just summaries. Use `ProjectContext::build_context_only_summary()` but increase its budget to the maximum allowed by the model context window. The LLM output is raw Markdown text (not Gherkin), so skip Gherkin-specific parsing.
>
> **In `src/app.rs`**: (a) Add field `markdown_results: HashMap<String, crate::markdown::MarkdownDocument>` to `DockOckApp`, initialize in `new()`. (b) Add `ProcessingEvent::MarkdownResult { file_name: String, document: crate::markdown::MarkdownDocument }` variant. (c) In `process_files()`, after the LLM returns raw output and `output_mode` is `Markdown`, parse it via `MarkdownDocument::parse_from_llm_output()` and send the event. (d) In `poll_events()`, handle `MarkdownResult` by inserting into `markdown_results`. (e) Fix every `match output_mode` exhaustiveness error by adding `OutputMode::Markdown => { ... }` arms — look at the parallel `Gherkin` and `DependencyGraph` arms to understand the pattern and replicate it for Markdown. In the button text match, use `"⚙ Generate Markdown"`. Run `cargo check`.

---

## Step 6 — Add UI Controls and Display Panel

**Goal:** Show Markdown output in the right panel when `OutputMode::Markdown` is selected, and save `.md` files to the output directory.

**Files:** `src/app.rs`

**UI changes:**

1. **Output mode dropdown**: Already works because `OutputMode::ALL` now includes `Markdown` (Step 1).

2. **Right panel rendering** (around line ~1912 where `DependencyGraph` displays JSON): Add a `Markdown` arm that displays the rendered markdown as plain text in a scrollable `egui::TextEdit::multiline` (read-only) or as raw text with monospace font. Since egui doesn't render Markdown natively, display it as formatted source text.

3. **File save**: When generation completes, save each file's result as `<output_dir>/<filename>.md`. This parallels the `.feature` file save for Gherkin mode. Add this in the section where Gherkin files are written to disk (search for `.feature` file writing in `app.rs`).

4. **Copy button**: The existing copy-to-clipboard button should work — just copy the raw Markdown string.

5. **Diff support**: Reuse the existing `diff_gherkin()` function (it's line-based, works on any text). Show diff when `previous_markdown_results` exist. Add `previous_markdown_results: HashMap<String, MarkdownDocument>` field.

**Prompt for implementing agent:**
> You are adding UI support for Markdown output mode in DockOck's `src/app.rs`. The `OutputMode::Markdown` variant and `MarkdownDocument` struct already exist. Make these changes:
>
> 1. **Right panel** (find the area around line ~1912 where `OutputMode::DependencyGraph` renders JSON): Add an `OutputMode::Markdown` arm. Render the Markdown text from `self.markdown_results.get(&selected_file)` in a read-only `egui::TextEdit::multiline` with monospace font (`egui::TextStyle::Monospace`). If no result exists, show "No markdown generated yet."
>
> 2. **File saving**: Find where `.feature` files are written to disk (search for `".feature"` in app.rs). Add a parallel branch: when `output_mode` is `Markdown`, save as `<output_dir>/<stem>.md` using `md_doc.to_markdown_string()`.
>
> 3. **Diff**: Add `previous_markdown_results: HashMap<String, crate::markdown::MarkdownDocument>` field to `DockOckApp`. Before regeneration (where `previous_results` is populated), also populate `previous_markdown_results`. Reuse `session::diff_gherkin()` on the markdown strings for the diff panel.
>
> 4. **Session data transfer**: In the session save/load, handle `markdown_results` (Step 7 will do full persistence — for now, just ensure the field exists and is initialized to empty on load).
>
> Run `cargo check`.

---

## Step 7 — Integrate Session Persistence and Caching

**Goal:** Persist `MarkdownDocument` results in the session file and cache LLM outputs.

**Files:** `src/session.rs`, `src/cache.rs`, `src/app.rs`

**Changes in `src/session.rs`:**

1. Add to `SessionData`:
   ```rust
   pub markdown_results: HashMap<String, crate::markdown::MarkdownDocument>,
   pub previous_markdown_results: HashMap<String, crate::markdown::MarkdownDocument>,
   ```

2. In `save()` and `load()`: These are already generic (serde JSON) — adding the fields is sufficient. Use `#[serde(default)]` on the new fields for backward compatibility with existing session files.

**Changes in `src/cache.rs`:**

1. Add a new namespace constant:
   ```rust
   pub const NS_MARKDOWN: &str = "markdown";
   ```

2. In `src/llm/mod.rs`'s `process_file()`, when `output_mode == Markdown`:
   - Cache key: same composite hash as Gherkin mode (file + content + mode + models + context)
   - Cache value: the raw Markdown string
   - Use namespace `NS_MARKDOWN` instead of `NS_LLM`

**Changes in `src/app.rs`:**

1. Wire session save/load to include `markdown_results`.
2. On session load, populate `self.markdown_results` and `self.previous_markdown_results`.

**Prompt for implementing agent:**
> You are adding persistence and caching for the Markdown output mode in DockOck.
>
> **`src/session.rs`**: Add two fields to `SessionData`: `markdown_results: HashMap<String, crate::markdown::MarkdownDocument>` and `previous_markdown_results: HashMap<String, crate::markdown::MarkdownDocument>`. Add `#[serde(default)]` to both for backward compatibility with existing session files that don't have these fields.
>
> **`src/cache.rs`**: Add `pub const NS_MARKDOWN: &str = "markdown";` alongside the existing `NS_PARSED`, `NS_VISION`, `NS_LLM`, etc.
>
> **`src/llm/mod.rs`**: In `process_file()`, when `output_mode` is `Markdown`, use `cache::NS_MARKDOWN` for the cache namespace instead of `NS_LLM`. The cache key computation stays the same (composite hash of file content, mode, models, context).
>
> **`src/app.rs`**: In the session save path (search for `session::save`), include `markdown_results` and `previous_markdown_results`. In the session load path (search for `session::load`), restore them into `self.markdown_results` and `self.previous_markdown_results`. Initialize both to `HashMap::new()` in `DockOckApp::new()`.
>
> Run `cargo check`.

---

## Step 8 — Cross-Document Relationship Graph in Markdown

**Goal:** When multiple files are processed in Markdown mode, generate a cross-document relationship summary and inject it into each file's Markdown, and also create a project-level index Markdown file.

**Files:** `src/markdown.rs`, `src/app.rs`

**New function in `src/markdown.rs`:**

```rust
/// Generate a project-level index document that maps relationships between all generated documents.
pub fn generate_project_index(documents: &HashMap<String, MarkdownDocument>) -> MarkdownDocument {
    // Title: "Project Knowledge Base Index"
    // Summary: lists all documents
    // Section "Document Inventory": table of all docs with title, source files, section count
    // Section "Cross-Reference Map": aggregated cross-references from all docs
    //   - For each doc, list what it references and what references it
    // Section "Entity Relationship Overview": merged ER info across all docs
    //   - Collect all EntityRelationship sections, deduplicate entities, list global relationships
    // Section "Shared Data Models": entities that appear in multiple documents
}
```

**Changes in `src/app.rs`:**

1. After all files are processed in Markdown mode, call `generate_project_index()`.
2. Save the index as `<output_dir>/_INDEX.md`.
3. For each individual Markdown file, ensure the `cross_references` field is populated by comparing entity names across documents.

**Optional enhancement — use DependencyGraph data:**
If the project already has `DependencyGraph` results, use node/edge data from `src/depgraph.rs` to enrich the cross-reference map. This means calling the existing depgraph extraction and converting the graph into Markdown relationship descriptions.

**Prompt for implementing agent:**
> You are adding cross-document relationship mapping to DockOck's Markdown output mode.
>
> **`src/markdown.rs`**: Add a public function `generate_project_index(documents: &HashMap<String, MarkdownDocument>) -> MarkdownDocument`. It creates a meta-document titled "Project Knowledge Base Index" containing: (a) a "Document Inventory" section with a Markdown table listing each document's title, source files, and number of sections; (b) a "Cross-Reference Map" section aggregating all `cross_references` from all documents into a unified list, showing both outgoing and incoming references per document; (c) a "Shared Entities" section that finds entity/model names appearing in multiple documents' `DataModel` or `EntityRelationship` sections (simple string matching on section body text). Use `SectionKind::Narrative` for all index sections.
>
> **`src/app.rs`**: After all files complete in Markdown mode (find the post-processing section where OpenSpec export happens for Gherkin), call `crate::markdown::generate_project_index(&self.markdown_results)` and save the result as `<output_dir>/_INDEX.md`. Log this as a `ProcessingEvent::Log` with message "Generated project index: _INDEX.md".
>
> Run `cargo check`.

---

## Step 9 — Tech Stack Configuration and Prompt Injection

**Goal:** Allow users to select a target technology stack from a config file. The selected stack is injected into every Markdown-mode LLM prompt so that generated documentation uses the correct SQL dialect, type system, framework names, and deployment topology — making the output directly actionable by an implementing agent.

**Files:** `src/tech_stack.rs` (new), `tech_stacks.json` (new), `src/llm/mod.rs`, `src/app.rs`, `src/session.rs`, `src/main.rs`

### 9a. Config File Schema — `tech_stacks.json`

Place this file in the project root (next to `custom_providers.json`). It defines named tech stack presets:

```json
{
  "stacks": {
    "dotnet-react-pg": {
      "name": "AdComms Standard",
      "description": "Enterprise .NET + React + PostgreSQL stack",
      "layers": {
        "backend_api": {
          "technology": ".NET",
          "version": "10",
          "language": "C#",
          "framework": "ASP.NET Core Minimal API",
          "orm": "Entity Framework Core",
          "patterns": ["Clean Architecture", "CQRS", "MediatR", "Repository Pattern"]
        },
        "frontend_spa": {
          "technology": "React",
          "version": "18/19",
          "bundler": "Vite 6.x",
          "styling": "Tailwind CSS v4",
          "component_library": "shadcn/ui",
          "state_management": "TanStack Query + Zustand",
          "language": "TypeScript"
        },
        "database": {
          "technology": "PostgreSQL",
          "version": "16",
          "migration_tool": "EF Core Migrations",
          "sql_dialect": "PostgreSQL"
        },
        "cache": {
          "technology": "Redis",
          "version": "7.x / 8.x",
          "usage": ["Session store", "Distributed cache", "Rate limiting"]
        },
        "identity": {
          "technology": "Keycloak",
          "version": "26.x",
          "protocol": "OpenID Connect / OAuth 2.0",
          "integration": "Microsoft.AspNetCore.Authentication.JwtBearer"
        },
        "containers": {
          "technology": "Docker + Docker Compose",
          "version": "v2",
          "orchestration": "docker-compose.yml",
          "registry": "Container Registry (configurable)"
        }
      }
    }
  }
}
```

Users can add more stacks (e.g., `"spring-angular-mysql"`, `"fastapi-vue-mongo"`) to this file.

### 9b. Rust Data Model — `src/tech_stack.rs`

```rust
// src/tech_stack.rs

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::path::Path;

/// Root of the tech_stacks.json config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStackConfig {
    pub stacks: HashMap<String, TechStack>,
}

/// A named technology stack preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStack {
    pub name: String,
    pub description: String,
    pub layers: TechStackLayers,
}

/// The technology layers that compose the stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStackLayers {
    #[serde(default)]
    pub backend_api: Option<LayerSpec>,
    #[serde(default)]
    pub frontend_spa: Option<LayerSpec>,
    #[serde(default)]
    pub database: Option<LayerSpec>,
    #[serde(default)]
    pub cache: Option<LayerSpec>,
    #[serde(default)]
    pub identity: Option<LayerSpec>,
    #[serde(default)]
    pub containers: Option<LayerSpec>,
}

/// Specification for one technology layer.
/// Uses a flat HashMap for flexibility — keys are tech-specific
/// (e.g., "technology", "version", "orm", "patterns", "sql_dialect").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerSpec {
    pub technology: String,
    #[serde(default)]
    pub version: Option<String>,
    /// All other key-value pairs (language, framework, orm, patterns, etc.)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl TechStackConfig {
    /// Load from a JSON file. Returns empty config if file doesn't exist.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("tech_stacks.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse tech_stacks.json: {e}");
                Self { stacks: HashMap::new() }
            }),
            Err(_) => Self { stacks: HashMap::new() },
        }
    }

    /// List of all stack keys for UI dropdown.
    pub fn stack_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.stacks.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Display name for a stack key.
    pub fn display_name(&self, key: &str) -> String {
        self.stacks.get(key)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| key.to_string())
    }
}

impl TechStack {
    /// Render this tech stack as a prompt-injection block that can be prepended
    /// to any LLM system message.
    pub fn to_prompt_block(&self) -> String {
        let mut out = String::new();
        out.push_str("=== TARGET TECHNOLOGY STACK ===\n");
        out.push_str(&format!("Stack: {} — {}\n\n", self.name, self.description));

        if let Some(ref be) = self.layers.backend_api {
            out.push_str(&format!("Backend API: {} {}\n", be.technology, be.version.as_deref().unwrap_or("")));
            for (k, v) in &be.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref fe) = self.layers.frontend_spa {
            out.push_str(&format!("Frontend SPA: {} {}\n", fe.technology, fe.version.as_deref().unwrap_or("")));
            for (k, v) in &fe.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref db) = self.layers.database {
            out.push_str(&format!("Database: {} {}\n", db.technology, db.version.as_deref().unwrap_or("")));
            for (k, v) in &db.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref ca) = self.layers.cache {
            out.push_str(&format!("Cache / Session Store: {} {}\n", ca.technology, ca.version.as_deref().unwrap_or("")));
            for (k, v) in &ca.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref id) = self.layers.identity {
            out.push_str(&format!("Identity & Auth: {} {}\n", id.technology, id.version.as_deref().unwrap_or("")));
            for (k, v) in &id.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref ct) = self.layers.containers {
            out.push_str(&format!("Container Runtime: {} {}\n", ct.technology, ct.version.as_deref().unwrap_or("")));
            for (k, v) in &ct.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }

        out.push_str("\nIMPORTANT: All generated documentation MUST target this specific stack:\n");
        out.push_str("- Database schemas: use the SQL dialect of the specified database engine\n");
        out.push_str("- Data models: use the type system and conventions of the backend language\n");
        out.push_str("- API contracts: use the patterns of the specified backend framework\n");
        out.push_str("- Frontend components: reference the specified UI framework and component library\n");
        out.push_str("- Architecture diagrams: show the actual technology names, not generic labels\n");
        out.push_str("- Auth flows: use the specified identity provider's protocol and endpoints\n");
        out.push_str("- Deployment: use the specified container runtime and orchestration\n");
        out.push_str("=== END TECHNOLOGY STACK ===\n\n");
        out
    }
}

/// Format a serde_json::Value for prompt display.
fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr.iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}
```

### 9c. Wire into App State and UI

**Changes in `src/app.rs`:**

1. Add fields to `DockOckApp`:
   ```rust
   tech_stack_config: crate::tech_stack::TechStackConfig,
   selected_tech_stack: Option<String>,  // key into tech_stack_config.stacks
   ```

2. In `DockOckApp::new()`: Load the config:
   ```rust
   let exe_dir = std::env::current_exe().ok()
       .and_then(|p| p.parent().map(|d| d.to_path_buf()))
       .unwrap_or_default();
   let tech_stack_config = crate::tech_stack::TechStackConfig::load(&exe_dir);
   ```

3. In the top bar UI (near the `output_mode` dropdown): Add a tech stack dropdown that is **only visible when `output_mode == Markdown`**:
   ```rust
   if self.output_mode == crate::llm::OutputMode::Markdown {
       ui.label("Tech Stack:");
       let current_label = self.selected_tech_stack.as_ref()
           .map(|k| self.tech_stack_config.display_name(k))
           .unwrap_or_else(|| "None (generic)".to_string());
       egui::ComboBox::from_id_salt("tech_stack_combo")
           .selected_text(&current_label)
           .show_ui(ui, |ui| {
               if ui.selectable_label(self.selected_tech_stack.is_none(), "None (generic)").clicked() {
                   self.selected_tech_stack = None;
               }
               for key in self.tech_stack_config.stack_keys() {
                   let name = self.tech_stack_config.display_name(&key);
                   let selected = self.selected_tech_stack.as_deref() == Some(&key);
                   if ui.selectable_label(selected, &name).clicked() {
                       self.selected_tech_stack = Some(key.clone());
                   }
               }
           });
   }
   ```

### 9d. Inject into LLM Prompts

**Changes in `src/llm/mod.rs`:**

1. Add `tech_stack: Option<TechStack>` parameter to `process_file()` (or pass it through `AgentOrchestrator`).

2. When `output_mode == Markdown` and a tech stack is selected, prepend the tech stack block to every preamble:
   ```rust
   let preamble = if output_mode == OutputMode::Markdown {
       let base = MARKDOWN_GENERATOR_PREAMBLE;
       match &tech_stack {
           Some(stack) => format!("{}\n{}", stack.to_prompt_block(), base),
           None => base.to_string(),
       }
   } else {
       GENERATOR_PREAMBLE.to_string()
   };
   ```

3. The same injection applies to `MARKDOWN_EXTRACTOR_PREAMBLE` and `MARKDOWN_REVIEWER_PREAMBLE`.

4. The tech stack block tells the LLM:
   - Database schemas → use PostgreSQL SQL dialect
   - Data models → use C# types and EF Core conventions
   - Frontend → reference React components, TypeScript interfaces, shadcn/ui components
   - API → use ASP.NET Core Minimal API patterns
   - Auth → use Keycloak OIDC endpoints and JWT bearer tokens
   - Deployment → use Docker Compose service definitions

### 9e. Persist Selection in Session

**Changes in `src/session.rs`:**

Add to `SessionData`:
```rust
#[serde(default)]
pub selected_tech_stack: Option<String>,
```

Save/load the selection alongside other session fields.

### 9f. Cache Key Impact

The tech stack key must be included in the LLM cache composite hash — if the user switches stacks and regenerates, the cache must miss:
```rust
let cache_key = composite_key(&[
    file_name, raw_text, mode_str, generator_model, extractor_model,
    reviewer_model, images_hash, context,
    &tech_stack_key.unwrap_or_default(),  // NEW: include tech stack
]);
```

### 9g. Example Prompt Output

When the "AdComms Standard" stack is selected, the LLM receives this block prepended to `MARKDOWN_GENERATOR_PREAMBLE`:

```
=== TARGET TECHNOLOGY STACK ===
Stack: AdComms Standard — Enterprise .NET + React + PostgreSQL stack

Backend API: .NET 10
  language: C#
  framework: ASP.NET Core Minimal API
  orm: Entity Framework Core
  patterns: Clean Architecture, CQRS, MediatR, Repository Pattern
Frontend SPA: React 18/19
  bundler: Vite 6.x
  styling: Tailwind CSS v4
  component_library: shadcn/ui
  state_management: TanStack Query + Zustand
  language: TypeScript
Database: PostgreSQL 16
  migration_tool: EF Core Migrations
  sql_dialect: PostgreSQL
Cache / Session Store: Redis 7.x / 8.x
  usage: Session store, Distributed cache, Rate limiting
Identity & Auth: Keycloak 26.x
  protocol: OpenID Connect / OAuth 2.0
  integration: Microsoft.AspNetCore.Authentication.JwtBearer
Container Runtime: Docker + Docker Compose v2
  orchestration: docker-compose.yml
  registry: Container Registry (configurable)

IMPORTANT: All generated documentation MUST target this specific stack:
- Database schemas: use the SQL dialect of the specified database engine
- Data models: use the type system and conventions of the backend language
- API contracts: use the patterns of the specified backend framework
- Frontend components: reference the specified UI framework and component library
- Architecture diagrams: show the actual technology names, not generic labels
- Auth flows: use the specified identity provider's protocol and endpoints
- Deployment: use the specified container runtime and orchestration
=== END TECHNOLOGY STACK ===
```

This causes the LLM to generate:
- `CREATE TABLE ... ` with PostgreSQL syntax (`SERIAL`, `TEXT`, `TIMESTAMPTZ`)
- C# class definitions with `[Required]`, `[MaxLength]` attributes
- `IServiceCollection` registration patterns
- React component specs with `<Button>` from shadcn/ui
- Keycloak realm/client configuration
- `docker-compose.yml` service definitions

**Prompt for implementing agent:**
> You are adding tech stack configuration to DockOck's Markdown output mode. This involves creating 3 things:
>
> **1. Config file `tech_stacks.json`** (project root, next to `custom_providers.json`): Create a JSON file with a `"stacks"` object. The first stack has key `"dotnet-react-pg"`, name `"AdComms Standard"`, description `"Enterprise .NET + React + PostgreSQL stack"`, and layers: `backend_api` (.NET 10, C#, ASP.NET Core Minimal API, EF Core, patterns: Clean Architecture/CQRS/MediatR/Repository Pattern), `frontend_spa` (React 18/19, Vite 6.x, Tailwind CSS v4, shadcn/ui, TanStack Query + Zustand, TypeScript), `database` (PostgreSQL 16, EF Core Migrations, PostgreSQL SQL dialect), `cache` (Redis 7.x/8.x, usage: Session store/Distributed cache/Rate limiting), `identity` (Keycloak 26.x, OpenID Connect/OAuth 2.0, Microsoft.AspNetCore.Authentication.JwtBearer), `containers` (Docker + Docker Compose v2).
>
> **2. Module `src/tech_stack.rs`**: Define `TechStackConfig` (stacks: HashMap<String, TechStack>), `TechStack` (name, description, layers: TechStackLayers), `TechStackLayers` (optional fields for backend_api, frontend_spa, database, cache, identity, containers — each `Option<LayerSpec>`), `LayerSpec` (technology: String, version: Option<String>, extra: HashMap<String, serde_json::Value> with #[serde(flatten)]). Add `TechStackConfig::load(dir: &Path) -> Self` that reads `tech_stacks.json` from the directory (returns empty config if file missing). Add `TechStack::to_prompt_block(&self) -> String` that renders a structured text block with all layer details, ending with instructions telling the LLM to use this specific stack for all generated output. Register `mod tech_stack;` in `src/main.rs`.
>
> **3. Wire into app + pipeline**: In `src/app.rs`: add `tech_stack_config: TechStackConfig` and `selected_tech_stack: Option<String>` to `DockOckApp`. Load config in `new()` from the exe directory. Add a ComboBox dropdown in the top bar visible only when `output_mode == Markdown`, listing all stack names plus a "None (generic)" option. In `src/session.rs`: add `selected_tech_stack: Option<String>` with `#[serde(default)]` to `SessionData`. In `src/llm/mod.rs`: when `output_mode == Markdown` and a tech stack is selected, prepend `stack.to_prompt_block()` to all three preambles (extractor, generator, reviewer). Add the tech stack key to the LLM cache composite hash.
>
> Run `cargo check`.

---

## Step 10 — End-to-End Test and Example

**Goal:** Create a test example that exercises the Markdown pipeline and tech stack config end-to-end.

**Files:** `examples/test_markdown.rs` (new)

**What the test should do:**

1. Construct a `ProjectContext` with mock file contents:
   - A Word document with business rules, process descriptions, and entity references
   - An Excel document with test data tables (sheet name, column headers, rows)
   - A Visio document with architecture diagram shapes and connections
2. Call the Markdown extractor prompt template with mock content → verify the output contains all expected sections
3. Parse the raw Markdown output via `MarkdownDocument::parse_from_llm_output()`
4. Assert:
   - `sections.len() > 0`
   - At least one section has `SectionKind::DatabaseSchema` or `SectionKind::DataModel`
   - `cross_references` is populated if cross-doc references exist in source
5. Render via `to_markdown_string()` and verify it starts with `# ` and contains `## `
6. Call `generate_project_index()` with two documents and verify the index has inventory and cross-ref sections
7. Load a `TechStackConfig` from a temp dir with a sample `tech_stacks.json`. Verify it loads one stack. Call `to_prompt_block()` and verify the output contains "PostgreSQL", ".NET", and "React"

**Prompt for implementing agent:**
> You are writing an example/test for the Markdown knowledge-base feature in DockOck. Create `examples/test_markdown.rs`. This example should NOT require an LLM — it tests the parsing, rendering, and indexing logic only.
>
> 1. Construct a sample raw Markdown string that mimics LLM output: include `# Feature Title`, `## Summary`, `## Database Schema` (with a pipe table), `## Data Models`, `## Architecture` (with a ```mermaid block), `## Business Rules` (with BR-001 format), `## Test Data` (with a pipe table), `## Cross-References` (with `- **OtherDoc** (depends-on): shared entities`).
> 2. Call `dockock::markdown::MarkdownDocument::parse_from_llm_output(&sample, "test.docx")`.
> 3. Assert: title is "Feature Title", sections count >= 6, at least one section is `SectionKind::DatabaseSchema`, cross_references has at least 1 entry.
> 4. Call `to_markdown_string()` and assert the result contains `# Feature Title` and `## Database Schema`.
> 5. Create a second `MarkdownDocument` manually. Build a `HashMap` with both, call `dockock::markdown::generate_project_index(&map)`. Assert the index title is "Project Knowledge Base Index" and it has >= 2 sections.
> 6. Print "All Markdown knowledge base tests passed!" at the end.
>
> 7. Test tech stack: call `dockock::tech_stack::TechStackConfig::load()` with a temp dir containing a sample `tech_stacks.json`. Assert it loads one stack. Call `to_prompt_block()` and verify the output contains "PostgreSQL" and ".NET" and "React".
>
> Run `cargo check --example test_markdown`.

---

## Summary of All Prompt Templates

| Agent Role | Gherkin Prompt | Markdown Prompt | Key Differences |
|---|---|---|---|
| **Extractor** | `EXTRACTOR_PREAMBLE` — produces ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES, etc. (800 word limit) | `MARKDOWN_EXTRACTOR_PREAMBLE` — produces DATABASE SCHEMA, DATA MODELS, ARCHITECTURE, ENTITY RELATIONSHIPS, STATE MACHINES, BUSINESS RULES, PROCESS FLOWS, TEST DATA, UI/SCREENS, IMAGE/DIAGRAM (2000 word limit) | Markdown extractor captures richer technical detail: DB columns, Mermaid diagrams, API contracts, full test tables. Gherkin extractor focuses on behavioral summaries. |
| **Generator** | `GENERATOR_PREAMBLE` — outputs valid Gherkin syntax (Feature/Scenario/Given/When/Then) | `MARKDOWN_GENERATOR_PREAMBLE` — outputs structured Markdown with Mermaid diagrams, pipe tables, BR-NNN rules, field specifications | Entirely different output format. Markdown generator preserves raw data (tables verbatim) while Gherkin generator converts to behavioral steps. |
| **Reviewer** | `REVIEWER_PREAMBLE` — checks Gherkin syntax, step completeness, duplicate removal | `MARKDOWN_REVIEWER_PREAMBLE` — checks section completeness, Mermaid validity, table formatting, cross-reference coverage, data fidelity | Different quality criteria. Markdown reviewer validates technical documentation structure while Gherkin reviewer validates BDD syntax. |

---

## Architecture Decision Records

### ADR-1: Separate `MarkdownDocument` struct (not reusing `GherkinDocument`)
**Rationale:** Markdown knowledge bases have fundamentally different structure (sections, diagrams, tables) vs Gherkin (Features, Scenarios, Steps). A shared type would be awkward and error-prone.

### ADR-2: Full Context-only file inclusion in Markdown mode
**Rationale:** Gherkin mode summarizes Excel/Visio as supplementary context. Markdown mode must include their full content because the goal is comprehensive knowledge capture — every table row, every Visio shape.

### ADR-3: Mermaid diagrams for architecture/ER/state machines
**Rationale:** Mermaid is renderable in most Markdown viewers (GitHub, VS Code, Obsidian) and parseable by AI agents. It's the best portable format for diagrams in Markdown.

### ADR-4: Project-level `_INDEX.md` file
**Rationale:** When processing many documents, a cross-reference index helps AI agents understand the full project scope and navigate between related documents efficiently.

### ADR-5: Tech stack as prompt injection (not post-processing)
**Rationale:** Injecting the tech stack into the LLM system prompt at generation time (rather than post-processing Markdown output) produces far better results. The LLM can natively generate PostgreSQL DDL, C# classes, React TypeScript interfaces, and Keycloak config — rather than us trying to regex-transform generic schemas after the fact. The `to_prompt_block()` format is structured text (not JSON) because LLMs parse structured text more reliably in system prompts.

### ADR-6: External `tech_stacks.json` config file
**Rationale:** Using an external JSON file (same pattern as `custom_providers.json`) allows users to define custom stacks without recompiling. Teams can share stack definitions and add project-specific stacks (e.g., Java + Angular, Python + Vue). The file lives next to the executable for easy discovery.

---

## File Change Summary

| File | Action | Description |
|------|--------|-------------|
| `src/llm/mod.rs` | Modify | Add `OutputMode::Markdown` variant; add 3 prompt constants; wire pipeline branching; prepend tech stack block to Markdown preambles; add tech stack key to cache hash |
| `src/markdown.rs` | Create | `MarkdownDocument` struct, `Section`, `SectionKind`, `CrossReference`; rendering and parsing; `generate_project_index()` |
| `src/tech_stack.rs` | Create | `TechStackConfig`, `TechStack`, `TechStackLayers`, `LayerSpec`; JSON loading; `to_prompt_block()` rendering |
| `tech_stacks.json` | Create | Default tech stack presets (first entry: AdComms Standard — .NET 10, React, PostgreSQL 16, Redis, Keycloak 26.x, Docker Compose) |
| `src/main.rs` | Modify | Add `mod markdown;` and `mod tech_stack;` |
| `src/app.rs` | Modify | Add `markdown_results` field; add `ProcessingEvent::MarkdownResult`; wire UI panel, file saving, session save/load, button text; add `tech_stack_config` + `selected_tech_stack` fields; add tech stack ComboBox dropdown (visible only in Markdown mode) |
| `src/session.rs` | Modify | Add `markdown_results`, `previous_markdown_results`, and `selected_tech_stack` fields to `SessionData` |
| `src/cache.rs` | Modify | Add `NS_MARKDOWN` constant |
| `examples/test_markdown.rs` | Create | End-to-end parsing/rendering/tech-stack test |
| `docs/FEATURE_MARKDOWN_KNOWLEDGE_BASE.md` | Create | This plan document |
