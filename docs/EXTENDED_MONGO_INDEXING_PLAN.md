# Extended MongoDB Indexing — Feature Plan

> **Goal**: Dramatically increase the breadth of data indexed in MongoDB so that
> Chat, MCP, and cross-file RAG retrieval can answer questions about *generated*
> artefacts — not only raw source text.
>
> **Constraint**: All new collections follow the existing embedding + vector-index
> pattern established in `src/rag.rs`. All async work respects CancellationToken /
> TaskTracker structured-concurrency patterns.

---

## 1  Current State

### 1.1  What Is Indexed Today

| Collection | Content | Stored Fields | Indexed By |
|-----------|---------|---------------|------------|
| `chunks` | 1024-char overlapping windows of parsed document text (Word / Excel / Visio) | `_id`, `text`, `embedding` | `vector_index` (cosine) |
| `memories` | Factoids extracted from LLM-generated Gherkin output | `_id`, `text`, `embedding`, `run_id`, `source`, `created_at` | `memory_vector_index` (cosine) |

### 1.2  What Is Generated but *Not* Indexed

| Data | Where It Lives Today | Why It Matters |
|------|---------------------|----------------|
| **Gherkin scenarios** | `.feature` files on disk + `SessionData.results` | Steps encode tested business logic — Chat should be able to answer "which scenarios test the deletion rule?" |
| **Dependency graph nodes** | `.depgraph.json` on disk + `SessionData.depgraph_results` | Entities, states, transitions, and business rules are structured knowledge — ideal for precise retrieval |
| **Markdown KB sections** | `.md` files on disk + `SessionData.markdown_results` | Typed sections (DatabaseSchema, DataModel, BusinessRules, etc.) carry rich domain knowledge |
| **Image descriptions** | Folded into `raw_text` before chunking — identity lost | Diagram flows, reviewer comments, XML hierarchies from embedded images are valuable on their own |
| **Extracted entities** | `ProjectContext.entities` (in-memory, discarded after run) | Named actors, systems, and data objects enable entity-centric retrieval |
| **Cross-document references** | Implicit in Markdown `CrossReference` structs, never persisted | Enable "which documents reference entity X?" queries |

---

## 2  Design: New Collections

### 2.1  Collection Overview

Six new MongoDB collections extend the existing two:

```
dockock (database)
├── chunks              ← existing
├── memories            ← existing
├── scenarios           ← NEW: Gherkin scenarios
├── entities            ← NEW: Dependency graph nodes
├── sections            ← NEW: Markdown KB sections
├── images              ← NEW: Vision-extracted image descriptions
├── business_rules      ← NEW: Structured business rules
└── cross_references    ← NEW: Inter-document links
```

Each new collection stores:
- `_id` — deterministic ID derived from source + content (for upsert idempotency)
- `text` — the embeddable text payload (what the embedding model sees)
- `embedding` — vector embedding (dimensionality matches current provider)
- **Metadata fields** — structured data for filtering (see per-collection schemas below)

Each collection gets its own `vectorSearch` index (cosine similarity, dynamic
dimensions matching the configured embedding provider).

### 2.2  `scenarios` — Gherkin Scenarios

**Indexed after**: Gherkin generation completes (post Phase 2, per-file)

**Schema**:
```json
{
  "_id": "D028.docx:scenario:Login_with_valid_credentials",
  "text": "Feature: User Authentication\n  Scenario: Login with valid credentials\n    Given a registered user with valid credentials\n    When the user submits the login form\n    Then the user is redirected to the dashboard",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "feature_title": "User Authentication",
  "scenario_title": "Login with valid credentials",
  "is_outline": false,
  "step_count": 3,
  "keywords_used": ["Given", "When", "Then"],
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Embeddable text**: The full scenario rendered as `.feature` syntax (title +
all steps). This gives the embedding model the complete semantic unit.

**ID strategy**: `"{source_file}:scenario:{scenario_title_slug}"` — allows
re-indexing a file to replace its scenarios without duplicates.

**Retrieval use-cases**:
- "Which scenarios test the cancellation rule?" → vector search over scenario text
- "Show me all scenarios from D028" → filter by `source_file`
- "Find scenario outlines with data tables" → filter `is_outline == true`

### 2.3  `entities` — Dependency Graph Nodes

**Indexed after**: Dependency graph generation completes

**Schema**:
```json
{
  "_id": "D028.docx:entity:OrderManagement",
  "text": "Entity: OrderManagement (System)\nDescription: Manages the lifecycle of customer orders from creation through fulfillment.\nStates: Draft → Submitted → Approved → Fulfilled → Closed\nRules: BR-001: Orders must have at least one line item. BR-002: Only managers can approve orders above $10,000.",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "entity_name": "OrderManagement",
  "entity_type": "System",
  "state_names": ["Draft", "Submitted", "Approved", "Fulfilled", "Closed"],
  "transition_count": 5,
  "rule_count": 2,
  "rule_ids": ["BR-001", "BR-002"],
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Embeddable text**: A flattened prose representation of the node — name,
type, description, state names, transition triggers, and rule descriptions
concatenated into a readable paragraph. This gives embeddings full semantic
coverage of the entity.

**Retrieval use-cases**:
- "What states can an Order be in?" → vector search finds the OrderManagement entity
- "Which entities have a Deletion transition?" → vector search + post-filter on state_names
- "Find all actors in the system" → filter by `entity_type == "Actor"`

### 2.4  `sections` — Markdown KB Sections

**Indexed after**: Markdown generation completes

**Schema**:
```json
{
  "_id": "D028.docx:section:Database_Schema",
  "text": "## Database Schema\n\nThe Orders table contains the following columns:\n- order_id (PK, UUID)\n- customer_id (FK → Customers)\n- status (ENUM: draft, submitted, approved)\n- total_amount (DECIMAL 10,2)\n- created_at (TIMESTAMP)",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "document_title": "Order Management System",
  "section_heading": "Database Schema",
  "section_kind": "DatabaseSchema",
  "depth": 1,
  "parent_heading": null,
  "char_count": 342,
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Embeddable text**: The section heading + body markdown rendered as plain text.
For sections exceeding `CHUNK_SIZE_CHARS` (1024), apply the same chunking
strategy used for raw documents, with `_id` suffixed by chunk index.

**Section kinds indexed** (from `SectionKind` enum):
- `Narrative`, `DatabaseSchema`, `DataModel`, `ArchitectureDiagram`,
  `EntityRelationship`, `StateMachine`, `TestData`, `ApiContract`,
  `BusinessRules`, `UiDescription`, `ImageContent`

**Retrieval use-cases**:
- "Show me all database schemas" → filter `section_kind == "DatabaseSchema"`
- "What data model does the payment system use?" → vector search over section text
- "Find API contracts" → filter `section_kind == "ApiContract"`

### 2.5  `images` — Vision-Extracted Image Descriptions

**Indexed after**: Vision extraction completes (before RAG chunking)

**Schema**:
```json
{
  "_id": "D028.docx:image:3",
  "text": "Architecture diagram showing a three-tier deployment: Web Server (IIS) → Application Server (.NET Core) → Database (SQL Server). Load balancer in front. Redis cache between App and DB layers. Reviewer comment bubble from 'John Smith': 'Need to add the message queue here'.",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "image_index": 3,
  "mime_type": "image/png",
  "alt_text": "System Architecture",
  "has_reviewer_comments": true,
  "has_diagram_content": true,
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Embeddable text**: The vision LLM's full description of the image. This is
currently folded into `raw_text` and loses its identity — indexing it separately
preserves its provenance and makes it independently searchable.

**Retrieval use-cases**:
- "Show me all architecture diagrams" → vector search for "architecture diagram"
- "What did reviewers comment on the requirements?" → filter `has_reviewer_comments`
- "Find images with data flow descriptions" → vector search

### 2.6  `business_rules` — Structured Business Rules

**Indexed after**: Dependency graph generation completes (extracted from `GraphNode.rules`)

**Schema**:
```json
{
  "_id": "D028.docx:rule:BR-001",
  "text": "BR-001: Orders must have at least one line item before they can be submitted. Category: Runtime. Lifecycle phases: Creation, Edit. Entity: OrderManagement.",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "rule_id": "BR-001",
  "description": "Orders must have at least one line item before they can be submitted.",
  "entity_name": "OrderManagement",
  "category": "Runtime",
  "lifecycle_phases": ["Creation", "Edit"],
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Embeddable text**: A prose sentence combining the rule ID, description,
category, lifecycle phases, and owning entity. This gives the embedding model
the full semantic context of the rule.

**Retrieval use-cases**:
- "What rules apply to order creation?" → vector search + filter `lifecycle_phases contains "Creation"`
- "List all Setup rules" → filter `category == "Setup"`
- "Which rules constrain the deletion lifecycle?" → vector search for "deletion"

### 2.7  `cross_references` — Inter-Document Links

**Indexed after**: Markdown generation completes (extracted from `MarkdownDocument.cross_references`)

**Schema**:
```json
{
  "_id": "D028.docx:xref:D029.docx:defines_data_model",
  "text": "D028 (Order Management Requirements) references D029 (Database Design) for the data model definition of the Orders and LineItems tables.",
  "embedding": [0.12, -0.34, ...],
  "source_file": "D028.docx",
  "target_file": "D029.docx",
  "reference_type": "defines_data_model",
  "description": "D029 defines the data model referenced in D028's business rules.",
  "run_id": "abc123",
  "created_at": 1711234567
}
```

**Retrieval use-cases**:
- "Which documents reference the payment system?" → vector search
- "What depends on D029?" → filter `target_file == "D029.docx"`
- "Show me the document dependency graph" → aggregate all cross_references

---

## 3  Implementation Phases

### Phase 1: Infrastructure — Multi-Collection Support

**Files**: `src/rag.rs`

**Changes**:
1. Generalize `connect_mongo()` to return a `MongoClient` (not a single collection)
   so callers can access any collection by name.
2. Add `ensure_collection_index(collection, index_name, dims)` — a generic
   version of the existing `ensure_search_indexes()` that works for any
   collection + index name pair.
3. Add a `CollectionConfig` struct:
   ```rust
   pub struct CollectionConfig {
       pub name: &'static str,
       pub index_name: &'static str,
   }

   pub const CHUNKS: CollectionConfig       = CollectionConfig { name: "chunks",           index_name: "vector_index" };
   pub const MEMORIES: CollectionConfig      = CollectionConfig { name: "memories",         index_name: "memory_vector_index" };
   pub const SCENARIOS: CollectionConfig     = CollectionConfig { name: "scenarios",        index_name: "scenario_vector_index" };
   pub const ENTITIES: CollectionConfig      = CollectionConfig { name: "entities",         index_name: "entity_vector_index" };
   pub const SECTIONS: CollectionConfig      = CollectionConfig { name: "sections",         index_name: "section_vector_index" };
   pub const IMAGES: CollectionConfig        = CollectionConfig { name: "images",           index_name: "image_vector_index" };
   pub const BUSINESS_RULES: CollectionConfig = CollectionConfig { name: "business_rules", index_name: "rule_vector_index" };
   pub const CROSS_REFS: CollectionConfig    = CollectionConfig { name: "cross_references", index_name: "xref_vector_index" };
   ```
4. Add `build_collection_index()` — a generic function that takes a collection
   config, a list of `(id, text)` pairs, embeds them, upserts them, and ensures
   the vector index exists. This avoids duplicating the embed → upsert → index
   pattern for each collection.

**Estimated scope**: ~150 lines of new code in `rag.rs`.

---

### Phase 2: Scenario Indexing

**Files**: `src/rag.rs`, `src/app.rs`

**Changes**:
1. Add `index_scenarios()` in `rag.rs`:
   - Accepts `Vec<GherkinDocument>` + embedding model + `MongoClient`
   - For each scenario in each document: render to `.feature` syntax, build
     `(id, text)` pair, attach metadata fields
   - Call `build_collection_index()` with `SCENARIOS` config
2. In `app.rs`, after the Gherkin per-file tasks complete (where `save_feature_file()`
   is called), collect all `GherkinDocument` results and call `index_scenarios()`.
3. Wire up `CancellationToken` for the indexing call.

**Retrieval integration**:
- Extend `retrieve_full_context()` to optionally include scenario hits
  alongside chunks and memories.
- Add a `retrieve_scenarios()` function for Chat/MCP use.

---

### Phase 3: Entity & Business Rule Indexing

**Files**: `src/rag.rs`, `src/app.rs`, `src/depgraph.rs`

**Changes**:
1. Add `index_dependency_graph()` in `rag.rs`:
   - Accepts `Vec<DependencyGraph>` + embedding model + `MongoClient`
   - For each `GraphNode`: build a flattened prose representation of the entity,
     embed and upsert into `ENTITIES` collection
   - For each `BusinessRule` on each node: build a prose sentence, embed and
     upsert into `BUSINESS_RULES` collection
2. Add helper in `depgraph.rs`:
   - `GraphNode::to_embeddable_text() -> String` — renders the node as a
     readable paragraph for embedding
   - `BusinessRule::to_embeddable_text(entity_name: &str) -> String` — renders
     the rule with its owning entity context
3. In `app.rs`, after dependency graph tasks complete (where `write_depgraph_files()`
   is called), collect all `DependencyGraph` results and call
   `index_dependency_graph()`.

---

### Phase 4: Markdown Section Indexing

**Files**: `src/rag.rs`, `src/app.rs`, `src/markdown.rs`

**Changes**:
1. Add `index_markdown_sections()` in `rag.rs`:
   - Accepts `Vec<MarkdownDocument>` + embedding model + `MongoClient`
   - Flatten all sections (including subsections, recursively) into
     `(id, heading + body text)` pairs with metadata
   - For sections exceeding `CHUNK_SIZE_CHARS`, apply `chunk_text()` and index
     each chunk separately with a suffixed ID
   - Upsert into `SECTIONS` collection
2. Add `index_cross_references()`:
   - Extract `CrossReference` structs from each `MarkdownDocument`
   - Build prose descriptions, embed, upsert into `CROSS_REFS` collection
3. Add helper in `markdown.rs`:
   - `Section::to_embeddable_text() -> String` — heading + body, strips
     markdown formatting for cleaner embeddings
   - `flatten_sections(&[Section]) -> Vec<(String, &Section, Option<&str>)>` —
     recursive flattener returning `(path, section, parent_heading)`
4. In `app.rs`, after markdown tasks complete, collect all `MarkdownDocument`
   results and call both indexing functions.

---

### Phase 5: Image Description Indexing

**Files**: `src/rag.rs`, `src/app.rs`

**Changes**:
1. Add `index_image_descriptions()` in `rag.rs`:
   - Accepts a list of `(source_file, image_index, mime_type, alt_text, description)`
     tuples + embedding model + `MongoClient`
   - Embed each description and upsert into `IMAGES` collection
   - Detect `has_reviewer_comments` and `has_diagram_content` via simple
     heuristics (keyword presence: "reviewer", "comment", "diagram", "flow")
2. In `app.rs`, capture vision descriptions *before* they're folded into
   `raw_text`. The existing vision extraction loop already has the individual
   descriptions — store them in a side-channel `Vec` and pass to
   `index_image_descriptions()` after all vision work completes.

**Note**: This requires a small refactor to the vision extraction loop to
retain individual image descriptions alongside merging them into the file text.

---

### Phase 6: Unified Retrieval

**Files**: `src/rag.rs`, `src/chat.rs`

**Changes**:
1. Add `retrieve_from_collection()` — a generic vector-search function that
   works against any collection + index name pair:
   ```rust
   pub async fn retrieve_from_collection(
       client: &MongoClient,
       config: &CollectionConfig,
       query_embedding: &[f32],
       top_k: usize,
       filters: Option<Document>,
       cancel: &CancellationToken,
   ) -> Result<Vec<(f64, String, Document)>>
   ```
2. Add `retrieve_extended_context()` — a composite retrieval function that
   queries multiple collections in parallel and merges results by relevance:
   ```rust
   pub async fn retrieve_extended_context(
       client: &MongoClient,
       query_text: &str,
       embedding_model: &dyn EmbeddingModel,
       sources: &[CollectionConfig],  // which collections to search
       top_k_per_source: usize,
       max_chars: usize,
       exclude_file: Option<&str>,
       cancel: &CancellationToken,
   ) -> Result<String>
   ```
   This replaces `retrieve_full_context()` as the primary retrieval entry point.
3. Update `ChatEngine::retrieve_chunks()` in `chat.rs` to use
   `retrieve_extended_context()` with all collections enabled, giving Chat full
   visibility into scenarios, entities, rules, sections, and images.
4. Update the MCP server's retrieval handler similarly.

---

### Phase 7: Incremental Re-indexing

**Files**: `src/rag.rs`, `src/app.rs`

**Changes**:
1. Track which files have been indexed per collection via a `run_id` field on
   every document. When re-processing a file:
   - Delete all documents with `source_file == X` in each collection
   - Re-index the new outputs
2. Add `cleanup_stale_documents()`:
   - After a run completes, for each collection, delete documents whose
     `source_file` is not in the current file set (handles removed files).
3. The existing `existing_ids()` check in `build_index()` already provides
   incremental indexing for `chunks` — extend this pattern to all new collections.

---

### Phase 8: Index-on-Demand for Chat / MCP

**Files**: `src/app.rs`, `src/chat.rs`

**Changes**:
1. When `OutputMode::IndexOnly` is selected, the current flow parses and indexes
   chunks. Extend this to also:
   - Run a lightweight extraction pass (EXTRACTOR_PREAMBLE only, no full
     pipeline) to produce entities and business rules for indexing
   - Index any available session artefacts (previous Gherkin, depgraph, markdown
     results from `SessionData`) that haven't been indexed yet
2. Add a "Re-index session" button that indexes all existing session outputs
   into the new collections without re-running the LLM pipeline.

---

## 4  Vector Index Configuration

All new indexes use the same pattern as existing ones:

```json
{
  "name": "<index_name>",
  "type": "vectorSearch",
  "definition": {
    "fields": [{
      "type": "vector",
      "path": "embedding",
      "numDimensions": <dynamic>,
      "similarity": "cosine"
    }]
  }
}
```

Dimensions are determined at runtime based on the active embedding provider
(768 for `nomic-embed-text`, 1024 for `mxbai-embed-large`, 384 for FastEmbed).

The existing `ensure_search_indexes()` / `wait_for_search_index_ready()` logic
is reused via the generic `ensure_collection_index()` function from Phase 1.

---

## 5  Data Volume Estimates

Assuming a typical project with 20 source documents:

| Collection | Estimated Documents | Avg Text Size | Notes |
|-----------|-------------------|---------------|-------|
| `chunks` (existing) | ~2,000 | 1 KB | 100 chunks × 20 files |
| `memories` (existing) | ~200 | 0.2 KB | ~10 factoids per run |
| `scenarios` | ~400 | 0.5 KB | ~20 scenarios × 20 files |
| `entities` | ~100 | 1 KB | ~5 entities × 20 files |
| `business_rules` | ~300 | 0.3 KB | ~3 rules × 5 entities × 20 files |
| `sections` | ~600 | 1.5 KB | ~6 sections × 20 files (with sub-chunks) |
| `images` | ~80 | 0.5 KB | ~4 images × 20 files |
| `cross_references` | ~60 | 0.2 KB | ~3 refs × 20 files |
| **Total** | **~3,740** | | ~4× the current count |

Embedding cost at 64-batch rate remains manageable — the additional ~1,540
documents add approximately 24 extra embedding batches per full run.

---

## 6  Migration & Backwards Compatibility

- **No schema migration needed**: New collections are created on first use via
  `ensure_collection_index()`. Old databases continue to work — the `chunks`
  and `memories` collections are untouched.
- **Graceful degradation**: If a new collection doesn't exist (e.g., user hasn't
  run in Markdown mode yet), `retrieve_extended_context()` skips that source
  silently.
- **Session compatibility**: The `SessionData` struct doesn't need changes —
  indexing reads from the existing result HashMaps.

---

## 7  Testing Strategy

1. **Unit tests**: For each `to_embeddable_text()` helper — verify the output
   is deterministic and captures all semantic fields.
2. **Integration tests**: Stand up a `mongodb-atlas-local:7` container, index
   sample artefacts, verify vector search returns relevant results from each
   collection.
3. **End-to-end**: Process a small document set through the full pipeline,
   query Chat with questions that require cross-collection retrieval, verify
   answers reference the correct sources.

---

## 8  Phase Priority & Dependencies

```
Phase 1 (Infrastructure)
    │
    ├── Phase 2 (Scenarios)      ← Gherkin mode
    ├── Phase 3 (Entities/Rules) ← DependencyGraph mode
    ├── Phase 4 (Sections/Xrefs) ← Markdown mode
    └── Phase 5 (Images)         ← All modes (Vision)
         │
         ▼
    Phase 6 (Unified Retrieval)  ← requires all above
         │
         ▼
    Phase 7 (Incremental)        ← cleanup & efficiency
         │
         ▼
    Phase 8 (Index-on-Demand)    ← UX enhancement
```

Phases 2–5 are independent of each other and can be implemented in any order
or in parallel. Phase 6 requires at least one of them to be useful. Phases 7
and 8 are polish/efficiency improvements.

**Recommended order**: 1 → 2 → 3 → 6 → 4 → 5 → 7 → 8

This front-loads Gherkin scenario indexing (the most common output mode) and
entity/rule indexing (the highest-value structured data), then adds unified
retrieval before tackling the remaining collections.
