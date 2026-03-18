# Feature: Dependency Graph Generation

## Overview

Add a **Dependency Graph** output mode as an alternative to Gherkin generation.
The user picks one or the other before clicking **Generate** — the entire pipeline
(parsing → context → LLM → output) executes the same way but the LLM preambles,
output data-structures, and rendering change to produce a structured dependency
graph instead of `.feature` files.

This document is the implementation plan.

---

## 1. Concepts

### 1.1 What the graph captures

| Layer | Content |
|-------|---------|
| **Entities** | Actors, systems, services, data objects, modules discovered across all documents |
| **Business Rules** | Validation rules, constraints, invariants — each linked to the entity it governs |
| **State Transitions** | Lifecycle phases per entity (Created → Active → Suspended → Closed, etc.) including guards and triggers |
| **Dependencies** | Directed edges between entities: "Invoice *depends on* Vendor", "Meter Reading *triggers* Billing Run" |
| **Cross-doc Links** | Which documents contribute to which entities, enabling traceability |

### 1.2 Relationship to Gherkin mode

```
                  ┌────────────────────┐
  User picks ───► │   OutputMode       │
                  │  ● Gherkin         │
                  │  ○ DependencyGraph │
                  └────────┬───────────┘
                           │
         ┌─────────────────┴──────────────────┐
         │ Shared Pipeline                     │
         │  Phase 0: Endpoint probing          │
         │  Phase 1: Parallel file parsing     │
         │  Phase 1.25: Entity extraction      │
         │  Phase 1.35: KV-cache priming       │
         │  Phase 1.3: RAG index build         │
         └─────────────────┬──────────────────┘
                           │
              ┌────────────┴────────────┐
              │                         │
      ┌───────▼──────┐        ┌────────▼────────┐
      │ Gherkin path │        │ DepGraph path   │
      │  Extract     │        │  Analyse        │
      │  Generate    │        │  Graph-Gen      │
      │  Review      │        │  Graph-Review   │
      │  → .feature  │        │  → .depgraph    │
      └──────────────┘        └─────────────────┘
```

The context rules are **identical** for both paths:

- `ProjectContext` accumulation (cross-file excerpts or RAG retrieval)
- Entity glossary injection
- KV-cache prefix reuse (Ollama)
- Chunk-and-merge for oversized documents
- Vision image enrichment
- Context-only files (`FileRole::Context`) injected as reference data
- Group merging for multi-document groups

Only the **LLM preambles**, **output parser**, and **UI rendering** differ.

---

## 2. Data Model

### 2.1 New types — `src/depgraph.rs`

```rust
//! Dependency graph data-structures and formatting helpers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single state in an entity's lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub name: String,
    /// Human-readable description / entry conditions.
    pub description: String,
}

/// A transition between two states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub from_state: String,
    pub to_state: String,
    /// What triggers this transition (event, user action, timer, etc.)
    pub trigger: String,
    /// Guard conditions that must be true for the transition to fire.
    pub guards: Vec<String>,
}

/// A business rule attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessRule {
    pub id: String,
    pub description: String,
    /// Lifecycle phase(s) where this rule applies.
    pub lifecycle_phases: Vec<String>,
    /// Whether this is a setup/config rule or a runtime rule.
    pub category: RuleCategory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleCategory {
    Setup,
    Runtime,
}

/// A node in the dependency graph — represents one business entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    /// Unique identifier (slug derived from entity name).
    pub id: String,
    /// Human-readable name ("Invoice", "Vendor", "Meter Reading").
    pub name: String,
    /// Entity type classification.
    pub entity_type: EntityType,
    /// Short description of what this entity represents.
    pub description: String,
    /// Lifecycle states (if stateful).
    pub states: Vec<State>,
    /// State transitions (if stateful).
    pub transitions: Vec<Transition>,
    /// Business rules governing this entity.
    pub rules: Vec<BusinessRule>,
    /// Source documents that mention this entity.
    pub source_documents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityType {
    Actor,
    System,
    DataObject,
    Process,
    Service,
    ExternalSystem,
}

/// A directed edge between two graph nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from_node: String,
    pub to_node: String,
    pub relationship: EdgeRelationship,
    /// Optional label / description.
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeRelationship {
    DependsOn,
    Triggers,
    Contains,
    Produces,
    Consumes,
    Validates,
    Extends,
    References,
}

impl std::fmt::Display for EdgeRelationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DependsOn   => write!(f, "depends_on"),
            Self::Triggers    => write!(f, "triggers"),
            Self::Contains    => write!(f, "contains"),
            Self::Produces    => write!(f, "produces"),
            Self::Consumes    => write!(f, "consumes"),
            Self::Validates   => write!(f, "validates"),
            Self::Extends     => write!(f, "extends"),
            Self::References  => write!(f, "references"),
        }
    }
}

/// The complete dependency graph for a file or group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyGraph {
    /// Title of the graph (derived from Feature title or group name).
    pub title: String,
    /// All entity nodes.
    pub nodes: Vec<GraphNode>,
    /// All directed edges.
    pub edges: Vec<GraphEdge>,
    /// Source file(s) that produced this graph.
    pub source_files: Vec<String>,
}

impl DependencyGraph {
    /// Render as a Mermaid diagram string (for UI display & markdown export).
    pub fn to_mermaid(&self) -> String { /* see §5.2 */ }

    /// Render as a DOT (Graphviz) string.
    pub fn to_dot(&self) -> String { /* see §5.2 */ }

    /// Render as structured JSON (machine-readable export).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parse from raw LLM JSON output.
    pub fn parse_from_llm_output(raw: &str, source_files: &[&str]) -> Self { /* see §4 */ }
}
```

### 2.2 Relationship to existing types

| Existing type | Role in DepGraph mode |
|---|---|
| `ProjectContext` | **Unchanged** — same accumulation, same glossary, same cross-file summary |
| `FileContent` / `FileRole` | **Unchanged** — context-only files still injected as reference data |
| `GherkinDocument` | **Not used** — replaced by `DependencyGraph` in the result maps |
| `PipelineMode` | **Unchanged** — Fast/Standard/Full still controls how many LLM calls are made |
| `AgentOrchestrator` | **Extended** — new `process_file_depgraph()` and `process_group_depgraph()` methods |

---

## 3. Output Mode Selection

### 3.1 New enum — `src/llm/mod.rs`

```rust
/// What the pipeline produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// Generate Gherkin .feature files (current default).
    Gherkin,
    /// Generate dependency graphs with business logic and state transitions.
    DependencyGraph,
}

impl Default for OutputMode {
    fn default() -> Self { Self::Gherkin }
}

impl std::fmt::Display for OutputMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gherkin => write!(f, "Gherkin (.feature)"),
            Self::DependencyGraph => write!(f, "Dependency Graph"),
        }
    }
}

impl OutputMode {
    pub const ALL: [OutputMode; 2] = [Self::Gherkin, Self::DependencyGraph];
}
```

### 3.2 UI integration — `src/app.rs`

Add `output_mode: OutputMode` field to `DockOckApp`. Render a combo box in the top
bar next to the pipeline mode selector:

```
Pipeline: [Fast ▼]  Output: [Gherkin ▼]  Concurrency: ║ 3
```

When `OutputMode::DependencyGraph` is selected:
- The **Generate** button label changes to **"⚙ Generate Graph"**
- The right panel renders Mermaid instead of Gherkin text
- Save exports `.depgraph.json` + `.depgraph.md` (Mermaid) instead of `.feature`
- Refinement prompt changes to graph-aware instructions
- Diff view compares graph JSON (node/edge add/remove)

### 3.3 Process dispatch

In `process_files()` (the top-level async function in `app.rs`), branch on
`output_mode` when dispatching each work item:

```rust
match output_mode {
    OutputMode::Gherkin => {
        // existing path: orchestrator.process_file(...) or process_group(...)
    }
    OutputMode::DependencyGraph => {
        // new path: orchestrator.process_file_depgraph(...) or process_group_depgraph(...)
    }
}
```

A new `ProcessingEvent` variant carries the graph result:

```rust
ProcessingEvent::DepGraphResult {
    path: PathBuf,
    graph: DependencyGraph,
    elapsed: std::time::Duration,
}
ProcessingEvent::GroupDepGraphResult {
    group_name: String,
    graph: DependencyGraph,
    elapsed: std::time::Duration,
}
```

Result storage in `DockOckApp`:

```rust
depgraph_results: HashMap<PathBuf, DependencyGraph>,
group_depgraph_results: HashMap<String, DependencyGraph>,
```

---

## 4. LLM Pipeline — DepGraph Path

### 4.1 Preambles

The preambles follow the same structure as the Gherkin preambles but instruct the
LLM to output **structured JSON** conforming to the `DependencyGraph` schema.

#### DEPGRAPH_EXTRACTOR_PREAMBLE

Identical to `EXTRACTOR_PREAMBLE` — the extraction/analysis step is output-mode
agnostic (it produces a structured summary with ACTORS, PROCESSES, BUSINESS_RULES,
etc.). No change needed.

#### DEPGRAPH_GENERATOR_PREAMBLE

```text
You are an expert business analyst and systems architect.
Your task is to read a structured document summary and produce a dependency graph
capturing all business entities, their state lifecycles, business rules, and
inter-entity dependencies.

Rules:
1. Output ONLY valid JSON matching this schema (no prose before or after):
   {
     "title": "...",
     "nodes": [
       {
         "id": "snake_case_id",
         "name": "Human Name",
         "entity_type": "Actor|System|DataObject|Process|Service|ExternalSystem",
         "description": "...",
         "states": [{"name": "...", "description": "..."}],
         "transitions": [{"from_state": "...", "to_state": "...", "trigger": "...", "guards": ["..."]}],
         "rules": [{"id": "BR-001", "description": "...", "lifecycle_phases": ["Creation"], "category": "Setup|Runtime"}],
         "source_documents": ["D028_Req.docx"]
       }
     ],
     "edges": [
       {
         "from_node": "node_id",
         "to_node": "node_id",
         "relationship": "depends_on|triggers|contains|produces|consumes|validates|extends|references",
         "label": "optional description"
       }
     ]
   }
2. Every actor, system, data entity, and process mentioned in the summary MUST appear as a node.
3. For each entity with a lifecycle, enumerate ALL states and transitions with guards.
4. Business rules must reference the correct lifecycle phase (Creation, Edit, Category-change,
   Status-transition, Deletion).
5. Classify rules as Setup or Runtime — do NOT mix them.
6. Every dependency between entities MUST be captured as an edge with the correct relationship type.
7. source_documents tracks which input files contribute to each node (for traceability).
8. If the input contains "=== Embedded Image Descriptions ===", treat every image description
   as a first-class source of entities, states, transitions, and dependencies. Do NOT skip
   image-derived content.
9. FIELD SCOPING — Creation-phase rules must only reference fields from the Create/New dialog.
   FactBox and Consumer fields belong to separate nodes or edges.
10. Use concrete, business-readable names. No generic placeholders.
```

#### DEPGRAPH_REVIEWER_PREAMBLE

```text
You are a dependency graph quality reviewer.
Your task is to review and improve a JSON dependency graph.

Rules:
1. Fix any JSON syntax errors.
2. Ensure every node has at least one edge (no orphans unless truly independent).
3. Verify state transitions form valid, connected state machines (no unreachable states).
4. Check that business rules are attached to the correct nodes and lifecycle phases.
5. Remove duplicate nodes (same entity appearing twice with slightly different names).
6. Ensure edges use the correct relationship type.
7. Output ONLY the corrected JSON — no explanations.
8. If the graph is already good, return it unchanged.
9. SETUP vs RUNTIME CHECK — If a Setup rule is attached to a Runtime node, move it.
10. LIFECYCLE PHASE CHECK — Verify transitions match documented lifecycle phases.
```

#### DEPGRAPH_GROUP_GENERATOR_PREAMBLE / DEPGRAPH_GROUP_EXTRACTOR_PREAMBLE

Same pattern as the group Gherkin preambles — instruct the LLM to synthesise
entities from MULTIPLE documents into a single unified graph, merging overlapping
entities and deduplicating edges.

#### DEPGRAPH_MERGE_REVIEWER_PREAMBLE

For chunk-and-merge: instruct the LLM to merge multiple JSON graph fragments into
a single cohesive graph — combine duplicate nodes, unify state machines, deduplicate
edges.

### 4.2 Pipeline stages

The pipeline stages mirror Gherkin exactly — only the preamble and output parser change:

| Stage | Gherkin mode | DepGraph mode |
|-------|-------------|---------------|
| Extract/Preprocess | `EXTRACTOR_PREAMBLE` → structured summary | **Same** — reused as-is |
| Generate | `GENERATOR_PREAMBLE` → Gherkin text | `DEPGRAPH_GENERATOR_PREAMBLE` → JSON |
| Review | `REVIEWER_PREAMBLE` → corrected Gherkin | `DEPGRAPH_REVIEWER_PREAMBLE` → corrected JSON |
| Merge (chunked) | `MERGE_REVIEWER_PREAMBLE` → merged Gherkin | `DEPGRAPH_MERGE_REVIEWER_PREAMBLE` → merged JSON |

### 4.3 Context injection — unchanged

The following are **identical** regardless of output mode:

- `ProjectContext::build_summary()` / `build_summary_excluding()`
- `ProjectContext::build_context_only_summary()`
- `ProjectContext::build_glossary()`
- `ProjectContext::extract_entities()`
- RAG `dynamic_context()` via rig-core
- KV-cache prefix priming (preamble + glossary)
- Vision image enrichment (`enrich_text_with_images`)
- Input budget calculation (`model_input_budget`)
- Chunk-and-merge splitting (`needs_chunking` / `chunk_for_llm`)

### 4.4 New orchestrator methods

```rust
impl AgentOrchestrator {
    /// Run the dependency graph pipeline for one file.
    /// Mirrors process_file() but uses depgraph preambles and JSON output.
    pub async fn process_file_depgraph(
        &self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
        images: &[ExtractedImage],
        context: &ProjectContext,
        status_tx: &mpsc::Sender<String>,
        force_regenerate: bool,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Identical flow to process_file():
        // 1. Cache check (NS_DEPGRAPH namespace)
        // 2. Vision enrichment
        // 3. Chunking check
        // 4. Extract/Preprocess (SAME preamble)
        // 5. Generate (DEPGRAPH_GENERATOR_PREAMBLE)
        // 6. Review (DEPGRAPH_REVIEWER_PREAMBLE)
        // 7. Cache store
    }

    /// Run the dependency graph pipeline for a group.
    pub async fn process_group_depgraph(
        &self,
        group_name: &str,
        members: &[(String, String, String, Vec<ExtractedImage>)],
        context: &ProjectContext,
        status_tx: &mpsc::Sender<String>,
        force_regenerate: bool,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Identical flow to process_group() with depgraph preambles
    }

    // Internal helpers — thin wrappers that call the same
    // run_ollama_chat / run_openai_chat with different preambles:

    async fn generate_depgraph(...) -> Result<String>;
    async fn review_depgraph(...) -> Result<String>;
    async fn generate_group_depgraph(...) -> Result<String>;
    async fn merge_chunk_depgraph(...) -> Result<String>;
}
```

---

## 5. Output Parsing & Rendering

### 5.1 JSON Parser — `DependencyGraph::parse_from_llm_output()`

```rust
impl DependencyGraph {
    pub fn parse_from_llm_output(raw: &str, source_files: &[&str]) -> Self {
        // 1. Strip markdown code fences (```json ... ```) if present
        // 2. Find first '{' and last '}' — the LLM sometimes wraps JSON in prose
        // 3. serde_json::from_str::<DependencyGraph>(cleaned)
        // 4. Fallback: if JSON parse fails, build a minimal graph with a
        //    single "Unknown" node containing the raw text as description
        //    (same resilience pattern as GherkinDocument::parse_from_llm_output)
        // 5. Inject source_files into each node that lacks them
    }
}
```

### 5.2 Rendering

#### Mermaid (for UI + markdown export)

```rust
impl DependencyGraph {
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("graph TD\n");

        // Nodes with labels
        for node in &self.nodes {
            let shape = match node.entity_type {
                EntityType::Actor => format!("{}([{}])", node.id, node.name),
                EntityType::Process => format!("{}{{{{{}}}}} ", node.id, node.name),
                EntityType::DataObject => format!("{}[{}]", node.id, node.name),
                EntityType::System => format!("{}[[{}]]", node.id, node.name),
                _ => format!("{}[{}]", node.id, node.name),
            };
            out.push_str(&format!("  {}\n", shape));
        }

        // Edges
        for edge in &self.edges {
            let arrow = match edge.relationship {
                EdgeRelationship::DependsOn => "-->",
                EdgeRelationship::Triggers => "-.->",
                EdgeRelationship::Contains => "---",
                _ => "-->",
            };
            if edge.label.is_empty() {
                out.push_str(&format!("  {} {} {}\n", edge.from_node, arrow, edge.to_node));
            } else {
                out.push_str(&format!("  {} {}|{}| {}\n", edge.from_node, arrow, edge.label, edge.to_node));
            }
        }

        // State transition subgraphs for stateful entities
        for node in &self.nodes {
            if !node.states.is_empty() {
                out.push_str(&format!("\n  subgraph {}_lifecycle[{} Lifecycle]\n", node.id, node.name));
                out.push_str("    direction LR\n");
                for state in &node.states {
                    let state_id = format!("{}_{}", node.id, state.name.to_lowercase().replace(' ', "_"));
                    out.push_str(&format!("    {}[{}]\n", state_id, state.name));
                }
                for tr in &node.transitions {
                    let from_id = format!("{}_{}", node.id, tr.from_state.to_lowercase().replace(' ', "_"));
                    let to_id = format!("{}_{}", node.id, tr.to_state.to_lowercase().replace(' ', "_"));
                    let label = if tr.guards.is_empty() {
                        tr.trigger.clone()
                    } else {
                        format!("{} [{}]", tr.trigger, tr.guards.join(", "))
                    };
                    out.push_str(&format!("    {} -->|{}| {}\n", from_id, label, to_id));
                }
                out.push_str("  end\n");
            }
        }

        out
    }
}
```

#### DOT (Graphviz) — optional export

Similar to Mermaid but in DOT syntax for users who prefer Graphviz rendering.

#### JSON — machine-readable export

`serde_json::to_string_pretty(&self)` — already provided by derive.

### 5.3 File export

| Format | Extension | When |
|--------|-----------|------|
| JSON (canonical) | `.depgraph.json` | Always saved |
| Mermaid markdown | `.depgraph.md` | Always saved (```mermaid block) |
| DOT | `.depgraph.dot` | Optional (user toggle) |

---

## 6. Session Persistence

### 6.1 SessionData additions

```rust
/// Inside SessionData (session.rs):
pub depgraph_results: HashMap<String, DependencyGraph>,
pub group_depgraph_results: HashMap<String, DependencyGraph>,
pub output_mode: OutputMode,
```

Backward compatible: old session files that lack these fields will deserialize
with defaults (`HashMap::new()`, `OutputMode::Gherkin`).

### 6.2 Diffing

For dependency graphs, `diff_depgraph(old, new)` produces:
- **Added nodes** (new entities discovered)
- **Removed nodes** (entities no longer mentioned)
- **Modified nodes** (states/rules/transitions changed)
- **Added/removed edges** (dependency changes)

Rendered in the UI as a colour-coded list rather than line-level text diff.

---

## 7. Caching

New cache namespace `NS_DEPGRAPH` — separate from `NS_LLM` so Gherkin and DepGraph
caches don't collide. The cache key computation is identical (file name + content +
mode + models), and includes `"depgraph"` as an additional discriminator.

```rust
pub const NS_DEPGRAPH: &str = "depgraph";
```

---

## 8. Implementation Plan

### Phase 1: Core data model & parser

| # | Task | File(s) | Est. |
|---|------|---------|------|
| 1.1 | Create `src/depgraph.rs` with all types | `src/depgraph.rs` | S |
| 1.2 | Implement `DependencyGraph::parse_from_llm_output()` | `src/depgraph.rs` | S |
| 1.3 | Implement `to_mermaid()` and `to_json()` renderers | `src/depgraph.rs` | S |
| 1.4 | Add `mod depgraph;` to `src/main.rs` | `src/main.rs` | XS |
| 1.5 | Add `NS_DEPGRAPH` constant to `src/cache.rs` | `src/cache.rs` | XS |

### Phase 2: Output mode enum & orchestrator methods

| # | Task | File(s) | Est. |
|---|------|---------|------|
| 2.1 | Add `OutputMode` enum to `src/llm/mod.rs` | `src/llm/mod.rs` | XS |
| 2.2 | Add depgraph preamble constants | `src/llm/mod.rs` | S |
| 2.3 | Implement `process_file_depgraph()` | `src/llm/mod.rs` | M |
| 2.4 | Implement `process_group_depgraph()` | `src/llm/mod.rs` | M |
| 2.5 | Implement `generate_depgraph()`, `review_depgraph()`, `merge_chunk_depgraph()` | `src/llm/mod.rs` | M |

### Phase 3: App integration

| # | Task | File(s) | Est. |
|---|------|---------|------|
| 3.1 | Add `output_mode` field + UI combo box to `DockOckApp` | `src/app.rs` | S |
| 3.2 | Add `DepGraphResult` / `GroupDepGraphResult` variants to `ProcessingEvent` | `src/app.rs` | S |
| 3.3 | Add `depgraph_results` / `group_depgraph_results` storage | `src/app.rs` | S |
| 3.4 | Branch `process_files()` dispatch on `output_mode` | `src/app.rs` | M |
| 3.5 | Render Mermaid in right panel when DepGraph mode active | `src/app.rs` | M |
| 3.6 | Save/export `.depgraph.json` + `.depgraph.md` | `src/app.rs` | S |
| 3.7 | Update refinement to use graph-aware prompt | `src/app.rs` | S |

### Phase 4: Session & diffing

| # | Task | File(s) | Est. |
|---|------|---------|------|
| 4.1 | Extend `SessionData` with depgraph fields + `output_mode` | `src/session.rs` | S |
| 4.2 | Implement `diff_depgraph()` | `src/session.rs` or `src/depgraph.rs` | M |
| 4.3 | Render graph diff in UI (added/removed/changed nodes & edges) | `src/app.rs` | M |

### Phase 5: Polish & testing

| # | Task | File(s) | Est. |
|---|------|---------|------|
| 5.1 | Add example test: `examples/test_depgraph.rs` | `examples/` | S |
| 5.2 | Verify round-trip: parse → generate → review → render → export | manual | M |
| 5.3 | Verify session restore with depgraph results | manual | S |
| 5.4 | Verify cache isolation (Gherkin ≠ DepGraph) | manual | S |

---

## 9. Files Changed Summary

| File | Change type |
|------|-------------|
| `src/depgraph.rs` | **New** — data model, parser, renderers |
| `src/main.rs` | Modify — add `mod depgraph;` |
| `src/cache.rs` | Modify — add `NS_DEPGRAPH` |
| `src/llm/mod.rs` | Modify — `OutputMode`, preambles, `process_file_depgraph()`, `process_group_depgraph()` |
| `src/app.rs` | Modify — `output_mode` field, UI, dispatch branching, result storage, rendering, export |
| `src/session.rs` | Modify — extend `SessionData`, add `diff_depgraph()` |
| `examples/test_depgraph.rs` | **New** — integration test |

---

## 10. Context Feed Rules — Parity Guarantee

To ensure identical treatment, the following table confirms that every context
mechanism applies equally to both modes:

| Context mechanism | Gherkin | DepGraph |
|---|---|---|
| `ProjectContext::build_summary()` in chat history | ✅ | ✅ |
| `ProjectContext::build_summary_excluding()` for groups | ✅ | ✅ |
| `ProjectContext::build_context_only_summary()` for reference files | ✅ | ✅ |
| `ProjectContext::build_glossary()` entity injection | ✅ | ✅ |
| `ProjectContext::extract_entities()` heuristic extraction | ✅ | ✅ |
| RAG `dynamic_context()` via MongoDB vector store | ✅ | ✅ |
| KV-cache prefix priming (Ollama) | ✅ | ✅ |
| Vision `enrich_text_with_images()` | ✅ | ✅ |
| `FileRole::Context` files as reference-only | ✅ | ✅ |
| Chunk-and-merge for oversized documents | ✅ | ✅ |
| Group member exclusion from cross-file context | ✅ | ✅ |
| `model_input_budget()` / `context_window_for_model()` | ✅ | ✅ |
| Retry with exponential backoff on 429/503 | ✅ | ✅ |
| Cancellation token support | ✅ | ✅ |
| Semaphore concurrency control | ✅ | ✅ |

---

## 11. Non-Goals (out of scope)

- **Interactive graph editing** — the graph is read-only in v1; refinement uses LLM re-generation
- **Live graph visualization** — Mermaid text is rendered; interactive node-dragging is deferred
- **Graph merging across runs** — each run produces a fresh graph; manual merge is not supported
- **OpenSpec integration for graphs** — OpenSpec expects Gherkin; graph→OpenSpec is a separate feature
