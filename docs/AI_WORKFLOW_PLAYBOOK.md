# AI Workflow Playbook

> Status: Initial draft  
> Scope today: Phase 1 is detailed. Later phases are intentionally scaffolded and will be refined as you provide more process detail, tools, and screenshots.

## Purpose

This playbook describes how we take a project from zero implementation, where the only reliable inputs are project documents, and turn it into a fully functioning software system with:

- production code
- unit tests
- integration tests
- performance tests
- UI tests
- traceable links back to source documentation

The first workflow described here is the current front door into that system: transforming Word requirements documents into Gherkin features while using Excel and Visio files as semantic reference context.

## End-State Workflow

The complete workflow is intended to progress through these phases:

1. Documentation intake and normalization
2. Artifact generation and retrieval indexing from documentation
3. Architecture and implementation planning
4. Code generation and iterative refinement
5. Unit test generation and enforcement
6. Integration test generation, including Testcontainers-based environment tests
7. Performance test generation and budget validation
8. UI test generation and user-flow verification
9. Human review, defect fixing, and release hardening

At this stage, the playbook documents four output and retrieval paths built on the same ingestion pipeline:

- documentation to Gherkin executable specifications
- documentation to Markdown knowledge-base documents
- documentation to dependency graphs
- documentation to an index-only retrieval base for chat and MCP consumers

## Core Principles

- Documentation is the initial source of truth.
- AI agents should operate on normalized, structured context instead of raw, disconnected files.
- Requirements documents are primary inputs.
- Spreadsheets and diagrams are supporting evidence unless explicitly promoted to primary inputs.
- Every downstream artifact should be traceable back to the originating documentation.
- Generated artifacts should pass through explicit quality gates before they become source-of-truth inputs for the next phase.

## Phase Map

### Phase 1. Documentation Intake and Normalization

Goal: load heterogeneous project documentation into a machine-usable representation.

Inputs:

- Word documents for requirements and narrative specifications
- Excel workbooks for field definitions, reference tables, status matrices, enumerations, or environment data
- Visio diagrams for workflows, architectures, state transitions, and interaction diagrams

Outputs:

- parsed document content
- extracted tables and diagram text
- optional image descriptions
- normalized project context for agent consumption

### Phase 2. Artifact Generation and Retrieval Indexing

Goal: convert normalized documentation into durable artifacts and retrieval indexes that can drive later coding and testing workflows.

This is the most detailed section in the current version of the playbook.

### Phase 3. Architecture and Implementation Planning

Goal: turn approved executable specifications into architecture, slice definitions, delivery order, and coding prompts.

Planned outputs:

- bounded feature slices
- target architecture
- implementation backlog
- dependency graph
- coding agent work packets

### Phase 4. Code and Test Generation

Goal: generate production code and test suites from approved specifications and plans.

Planned outputs:

- application code
- unit tests
- integration tests
- performance tests
- UI tests

### Phase 5. Verification and Hardening

Goal: prove the generated system works under functional, integration, performance, and UI constraints.

Planned outputs:

- green test suites
- performance measurements
- reviewed diffs
- release candidate

## Detailed Workflow: Documentation to Gherkin

This workflow is for projects where the initial usable assets are documentation files rather than source code.

### Objective

Produce high-quality Gherkin features from requirements documents while using related Excel and Visio files as semantic reference context rather than independent feature generators.

### Why This Matters

In real projects, the narrative requirements often live in Word documents, while critical details are spread across:

- Excel sheets containing field lists, statuses, valid values, mapping tables, or configuration data
- Visio diagrams containing state machines, workflows, system interactions, or architecture relationships

If the AI agent only sees the Word document, the generated scenarios are usually incomplete. The supporting files provide the missing domain constraints. The workflow therefore treats Word as the primary specification source and uses Excel and Visio as RAG-backed context providers.

### Inputs and Roles

| File Type | Role | Expected Use |
|---|---|---|
| `.docx` | Primary | Generate Gherkin features from requirement narratives |
| `.xlsx`, `.xls`, `.ods` | Context | Supply reference data, enumerations, and structural detail |
| `.vsdx` | Context | Supply workflows, state transitions, and system relationships |

### Operator Workflow in DockOck

1. Open DockOck.
2. Select the output directory if generated artifacts should be saved.
3. Add primary Word documents to the Source set.
4. Add supporting Excel and Visio files as related project inputs.
5. Allow auto-grouping by name or manually organize related files if needed.
6. Choose the provider, model, and pipeline mode.
7. Generate Gherkin.
8. Review the per-document or per-group output.
9. Save the generated feature files.
10. Optionally export the result to OpenSpec for downstream planning artifacts.

### What the System Does Internally

#### Step 1. Parse and Normalize All Files

Each file is parsed into text that the agent can consume.

- Word files are read as structured document content.
- Excel files are read as worksheet and row data.
- Visio files are read as page, shape, label, connector, and diagram text data.

The result is a normalized project context built from heterogeneous documentation.

#### Step 2. Assign Primary vs Context Roles

The workflow distinguishes between files that should produce Gherkin and files that should only enrich the prompt.

- Word documents are primary inputs.
- Excel and Visio files are context-only inputs.

This matters because reference tables and diagrams often contain domain rules but do not represent end-user behavior in a form that should be converted directly into scenarios.

#### Step 3. Build Shared Project Context

The system accumulates parsed content across all loaded files.

This shared context is used to:

- preserve cross-document terminology
- build a project glossary
- expose related entities, actors, systems, and data objects
- provide context when a requirement references concepts defined elsewhere

#### Step 4. Build Retrieval Context with Semantic Search

Supporting Excel and Visio content is made available through semantic retrieval.

For each Word-to-Gherkin transformation, the system can retrieve the most relevant supporting chunks from other loaded documents, rather than injecting every file in full.

This is the key behavior for the workflow you described:

- the Word document drives the scenario generation
- the Excel and Visio files supply precise supplemental information
- semantic retrieval determines which supporting content is relevant for the current transformation

Examples:

- a Word requirement mentions a status transition, and retrieval pulls the matching Visio state flow
- a Word requirement mentions an asset type or field, and retrieval pulls the relevant Excel row or worksheet section
- a Word requirement describes a business flow, and retrieval pulls related diagram connectors or configuration values

#### Step 5. Run the LLM Pipeline

The active pipeline mode controls how much processing happens per work item.

| Mode | Intent |
|---|---|
| Fast | lower latency, typically a direct generation path |
| Standard | balanced quality and cost with more structure |
| Full | extraction, generation, and review passes for highest quality |

The current DockOck architecture supports a multi-step pipeline centered on:

1. extraction
2. generation
3. review

This pipeline is appropriate because documentation is often inconsistent, repetitive, or ambiguous. The extraction pass creates structured understanding, the generation pass emits Gherkin, and the review pass tightens syntax and coverage.

#### Step 6. Persist and Reuse Knowledge

The system can cache parsing and model results and also retain retrievable project memory, which reduces redundant work across repeated runs and helps later processing stay consistent.

#### Step 7. Review and Save the Result

The user reviews the generated feature text, optionally compares it with a golden version, and saves the output for downstream use.

### Output of This Phase

The output of this workflow is a set of Gherkin feature files that represent executable specifications derived from the project documentation.

These features become the starting point for later phases such as:

- architecture derivation
- implementation planning
- unit test generation
- integration test generation
- performance scenario definition
- UI flow validation

## Detailed Workflow: Documentation to Markdown Knowledge Base

This workflow uses the same document-ingestion and semantic-context pattern as the Gherkin path, but the output is a structured Markdown knowledge base rather than executable scenarios.

### Objective

Produce a reusable Markdown knowledge base from project documentation so downstream agents have a richer narrative and structural reference document in addition to, or instead of, Gherkin.

### When to Use Markdown Output

Markdown mode is useful when the target artifact should preserve broader explanatory context such as:

- architecture descriptions
- business and domain overviews
- glossary and terminology
- reference tables
- workflows and diagram summaries
- implementation context for coding agents

Where Gherkin is optimized for executable behavior, Markdown is optimized for durable project knowledge.

### Input Pattern

The input pattern is the same as the Gherkin flow:

- Word documents provide the primary narrative
- Excel contributes structured reference data
- Visio contributes workflow, architecture, or state information
- semantic retrieval pulls the most relevant supporting fragments into generation

### Operator Workflow in DockOck

1. Load the same project documentation set used for Gherkin generation.
2. Set the Output mode to Markdown (.md).
3. Choose provider, model, and pipeline mode.
4. Start generation.
5. Review the generated Markdown knowledge-base output.
6. Save individual Markdown files or the full generated set.
7. Optionally use the result as high-context input for downstream coding agents.

### What the System Produces

The Markdown path is designed to generate richer knowledge artifacts, including:

- executive summaries
- architectural descriptions
- domain entities and glossary material
- reference tables from spreadsheets
- diagram interpretations from Visio or image-derived content
- consolidated project indexes across generated documents

### Relationship Between Gherkin and Markdown Outputs

These two outputs are complementary, not competing:

- Gherkin captures the behavioral contract
- Markdown captures the broader knowledge context

In practice, a mature AI delivery workflow can use both:

1. Markdown provides architectural and domain grounding.
2. Gherkin provides executable acceptance criteria.
3. Coding agents use both artifacts together to produce more accurate implementations and tests.

## Detailed Workflow: Documentation to Dependency Graph

This workflow uses the same document-ingestion pipeline, but instead of generating scenarios or narrative knowledge articles, it extracts business entities, rules, transitions, and relationships into a dependency graph.

### Objective

Produce a machine- and human-readable dependency graph showing how business cases, entities, services, actors, rules, and lifecycle transitions relate across the documentation set.

### When to Use Dependency Graph Output

Dependency graph mode is useful when the team needs to understand:

- business-case relationships
- entity dependencies
- process triggers and downstream effects
- lifecycle states and transitions
- cross-document structural links
- architecture and implementation impact before coding starts

Where Gherkin expresses behavior and Markdown expresses knowledge, the dependency graph expresses structure and linkage.

### Input Pattern

The input pattern is still the same:

- Word documents provide the main business-case and requirement descriptions
- Excel contributes reference data, business-rule detail, and entity attributes
- Visio contributes flows, transitions, and system relationships
- semantic retrieval helps connect the relevant supporting context during graph generation

### Operator Workflow in DockOck

1. Load the documentation set.
2. Set the Output mode to Dependency Graph.
3. Start generation.
4. Review the generated graph summary inside DockOck.
5. Copy the JSON, Mermaid, or DOT representations if needed.
6. Open the visual browser view to inspect the rendered graph.
7. Save or reuse the result as a planning artifact for downstream design and coding work.

### What the System Produces

The dependency-graph path is designed to surface:

- entities such as actors, systems, services, data objects, and processes
- directed dependencies between those entities
- business rules attached to the relevant nodes
- lifecycle states and transitions where the documentation describes them
- a combined multi-source graph across the imported documentation set
- Mermaid-compatible graph data for visualization

### Why This Matters for AI Delivery

This output is useful before code generation because it exposes coupling and dependency structure that may not be obvious from raw documents.

That helps later phases answer questions such as:

- which business cases depend on the same shared services or data objects
- which upstream entities trigger downstream processing
- which validations or states constrain implementation design
- where test boundaries should be drawn for unit and integration tests

### Browser Visualization Step

Unlike the plain text outputs, the dependency graph can also be opened in a rendered browser view. That gives the team a fast way to inspect the Mermaid diagram visually, zoom it, and export it for discussion or review.

### Relationship to the Other Output Modes

The three output modes complement each other:

- Gherkin defines behavior
- Markdown captures domain and architecture context
- Dependency graph reveals structural and causal relationships

Together, they provide a stronger input package for downstream coding and test-generation agents.

## Detailed Workflow: Documentation to Index-Only Retrieval Base

This workflow uses the same parsing and normalization pipeline, but instead of generating a new visible artifact in the main output panel, it builds a persistent retrieval layer for downstream question answering.

### Objective

Take loaded project documents, vectorize their parsed content, and store the results in Mongo Atlas so the DockOck Chat UI and MCP server can serve semantic search and full-document retrieval to coding models.

### When to Use Index-Only Mode

Index-only mode is useful when the team wants to:

- ingest project documents without generating Gherkin or Markdown immediately
- build a searchable semantic knowledge base first
- let coding models query source material through chat or MCP tools
- persist parsed source content and any previously generated artifacts for later reuse
- separate document indexing from later generation workflows

Where Gherkin produces executable specifications, Markdown produces narrative knowledge, and dependency graph produces structural linkage, index-only mode produces retrieval infrastructure.

### Operator Workflow in DockOck

1. Load the source document set into DockOck.
2. Set the Output mode to IndexOnly.
3. Confirm Mongo Atlas connectivity and embedding configuration.
4. Start indexing.
5. Wait for parsing, chunking, embedding, and Mongo upsert operations to complete.
6. Open the Chat UI to query the indexed knowledge base semantically.
7. Optionally enable or connect to the MCP server so external coding agents can query the same indexed corpus.

### What the System Does Internally

#### Step 1. Parse and Normalize the Source Files

The same ingestion pipeline used by the generation modes extracts machine-usable text from Word, Excel, and Visio inputs.

- Word files contribute structured requirement and narrative text.
- Excel files contribute worksheet content, tables, enumerations, and reference values.
- Visio files contribute page labels, shapes, connectors, and workflow text.

#### Step 2. Persist Full Source Documents

The parsed source text is stored as full documents in MongoDB so downstream consumers can retrieve complete source material when they already know the filename they need.

This supports exact retrieval in addition to semantic search.

#### Step 3. Chunk and Vectorize Parsed Content

The normalized text is split into retrievable chunks, embedded with the configured embedding provider, and prepared for vector search.

This creates the semantic representation needed for natural-language querying rather than filename-only lookup.

#### Step 4. Store Vectors in Mongo Atlas

Embedded chunks are upserted into the Mongo Atlas vector-backed collections used by DockOck's RAG layer.

This gives the system a persistent cross-session retrieval base instead of a transient in-memory index.

#### Step 5. Expose the Indexed Corpus to Chat and MCP

Once indexing completes, the same stored content can be queried from two fronts:

- the built-in Chat UI for interactive question answering inside DockOck
- the MCP server for external coding models and agent clients

In both cases, the query path retrieves semantically relevant chunks from Mongo Atlas and can also fall back to full-document retrieval when the caller needs the complete stored source or previously generated artifact.

### What the System Produces

Index-only mode produces a persistent retrieval substrate rather than a new authored document.

That includes:

- vectorized document chunks stored for semantic search
- full parsed source documents stored for direct retrieval
- any previously generated Markdown or Gherkin session artifacts made available for full-document lookup
- a reusable project knowledge base for the chat panel and MCP tools

### Why This Matters for Coding Models

Coding models are more effective when they can query a stable project knowledge base instead of relying only on a single prompt window.

Index-only mode enables that by letting models:

- ask semantic questions across the indexed documentation set
- retrieve the most relevant chunks without loading every document into context
- fetch full documents when a precise source file is required
- reuse the same indexed corpus across UI sessions and external agent runs

This makes the indexed documentation set available as an operational context service for implementation, debugging, and planning workflows.

### Quality Gates for Documentation-Derived Specifications

Before a generated feature is promoted downstream, it should satisfy the following checks:

1. It reflects the behavior described in the primary Word document.
2. It incorporates relevant constraints from Excel and Visio context.
3. It uses domain terminology consistently.
4. It is valid Gherkin syntax.
5. It covers happy path, negative path, and validation behavior when those are present in the documentation.
6. It does not invent unsupported business rules.
7. The feature can be traced back to specific source documents.

## Screenshots for This Workflow

The screenshots in this section illustrate the operator flow through DockOck. Where an image file is already present in the repository, it is embedded directly. The remaining screenshots stay as placeholders until their files are added.

### Screenshot A. Empty Workspace Before Generation

Use this image near the start of the workflow section.

What it shows:

- the DockOck desktop UI before files are loaded
- the Source and Golden areas on the left
- the pipeline, provider, model, and output controls across the top
- the empty Gherkin output panel waiting for input

Suggested caption:

"DockOck before document ingestion. The user selects source documentation, chooses the provider and pipeline mode, and prepares the generation run."

Pending file: `docs/playbook-02-...`

### Screenshot B. Files Loaded and Pipeline Mode Selected

Use this image in the operator workflow section after file selection.

What it shows:

- multiple project documents loaded into the left panel
- automatic grouping behavior in action
- pipeline mode options visible as Fast, Standard, and Full
- the relationship between selected inputs and the output panel

Suggested caption:

"Project documentation loaded into DockOck. Related Word files are processed as primary inputs while supporting material provides retrieval context for generation."

Pending file: `docs/playbook-02-...`

### Screenshot C. Generated Gherkin Output

![DockOck Gherkin transformation workflow - completed generation](playbook-01-gherkin-transformation.png)

Completed generation state in DockOck. The run has finished successfully, the Gherkin output is visible, and processed source documents are marked complete in the file list.

What it shows:

- generated Gherkin displayed in the right panel
- successful processing state in the left-hand file list
- OpenSpec export enabled in the run configuration
- saved or export-ready results after generation completes

Suggested caption:

"Generated executable specification after processing completes. The output Gherkin is reviewed, saved, and can be sent downstream to OpenSpec or later coding agents."

### Screenshot D. Markdown Knowledge-Base Generation In Progress

![DockOck markdown knowledge-base generation](playbook-02-markdown-before-transformation.png)

DockOck running in Markdown output mode. The same source document set is being processed through the pipeline, but the target artifact is a Markdown knowledge base instead of a Gherkin feature file.

What it shows:

- Output mode switched to Markdown (.md)
- the right panel labeled Markdown Knowledge Base
- the same project files loaded on the left side
- active processing in progress for markdown generation
- markdown generation exposed as a first-class workflow, not a separate tool

Suggested caption:

"Markdown knowledge-base generation uses the same documentation-ingestion workflow as Gherkin, but produces reusable narrative project context for downstream AI agents."

### Screenshot E. Markdown Knowledge-Base Generation Complete

![DockOck markdown knowledge-base generation complete](playbook-03-markdown-after-transformation.png)

Completed markdown generation in DockOck. The right panel now shows the generated Markdown knowledge base, the left panel shows processed documents and the project index, and the status area confirms successful completion.

What it shows:

- markdown output rendered after processing finishes
- a generated project index in the file list
- successful completion messages in the bottom log area
- processed source documents marked complete
- the Markdown output mode operating as a durable knowledge-base generator

Suggested caption:

"Completed markdown knowledge-base generation. The resulting document set can be saved and reused as high-context project input for downstream architecture, coding, and testing agents."

### Screenshot F. Dependency Graph Before Rendering

![DockOck dependency graph before generation](playbook-04-dependency-graph-before.png)

DockOck in Dependency Graph mode before the combined graph is rendered. The output panel is prepared to show the merged graph result after processing completes.

What it shows:

- Output mode switched to Dependency Graph
- the right panel labeled Dependency Graph
- the imported project files on the left
- the placeholder state before the combined dependency graph appears

Suggested caption:

"Dependency graph mode uses the same document-ingestion workflow but targets structural business relationships instead of scenarios or narrative documentation."

### Screenshot G. Dependency Graph Generated in DockOck

![DockOck dependency graph generated](playbook-04-dependency-graph-after.png)

Completed dependency-graph generation inside DockOck. The graph summary is available in text form, with actions to copy JSON, Mermaid, or DOT output and to open the rendered visual view.

What it shows:

- combined dependency graph summary for multiple source documents
- copy actions for JSON, Mermaid, and DOT formats
- an Open Visual action to launch the rendered graph
- successful completion messages and graph statistics in the status area

Suggested caption:

"The generated dependency graph can be reviewed in structured text form and exported in multiple formats for downstream tooling and design analysis."

### Screenshot H. Dependency Graph Visualized in the Browser

![Dependency graph browser view](playbook-04-dependency-graph-view.png)

Rendered browser view of the generated Mermaid dependency graph. This visual layer helps the team inspect relationships between business cases, services, entities, and rules more quickly than reading the text summary alone.

What it shows:

- a rendered combined dependency graph across multiple sources
- visual nodes and edges connecting business entities and processes
- browser-based controls for fit, zoom, export, and summary switching
- a graph view suitable for architecture review and implementation planning

Suggested caption:

"Rendered dependency-graph visualization in the browser. This step turns extracted business relationships into an inspectable planning artifact for architecture, coding, and test design."

### Screenshot I. Index-Only Mode Before Indexing

![DockOck index-only mode before indexing](playbook-05-index-only-before.png)

DockOck prepared in IndexOnly mode before the indexing run starts. The document set is loaded, but the goal is persistent retrieval rather than generating a new visible artifact.

What it shows:

- Output mode switched to IndexOnly
- source documents loaded and ready for ingestion
- the UI configured for an indexing run rather than a generation run
- the state just before chunking, embedding, and Mongo persistence begin

Suggested caption:

"Index-only mode prepares the loaded documentation set for semantic indexing and persistent retrieval instead of immediate artifact generation."

### Screenshot J. Indexed Knowledge Queried in the Chat UI

![DockOck index-only retrieval in chat](playbook-05-index-only-chat.png)

The built-in Chat UI querying the indexed corpus after an index-only run. The response is grounded in semantically retrieved project content stored in Mongo Atlas.

What it shows:

- indexed project knowledge queried through DockOck chat
- semantic retrieval feeding the answer path
- source-backed responses available without rerunning document generation
- the indexed corpus serving as reusable context for coding and analysis questions

Suggested caption:

"After indexing completes, the Chat UI can query the Mongo-backed semantic knowledge base directly, giving coding models grounded answers from the imported documentation set."

### Screenshot K. Indexed Knowledge Queried Through MCP

![DockOck index-only retrieval through MCP](playbook-05-index-only-mcp.png)

The same index-only knowledge base exposed through the MCP server so external coding agents can query it programmatically.

What it shows:

- MCP-based access to the indexed document corpus
- the same semantic retrieval layer used by the built-in chat experience
- external agent consumption of the Mongo-backed knowledge base
- index-only mode acting as a shared context service for coding workflows

Suggested caption:

"The MCP server exposes the same indexed corpus to external coding agents, so semantic and full-document retrieval are available outside the DockOck UI as well."

## How These Outputs Feed the Rest of the Delivery Workflow

These documentation-derived outputs are not the end product. They form the specification and planning bootstrap phase that makes later AI coding work more reliable.

The intended downstream chain is:

1. Gherkin features become the canonical executable requirements.
2. Markdown knowledge-base documents provide durable project and architecture context.
3. Dependency graphs expose structural relationships, coupling, and process dependencies.
4. Index-only mode provides persistent semantic and full-document retrieval for chat and MCP consumers.
5. Architecture and implementation prompts are generated from the approved artifact set and indexed knowledge base.
6. Coding agents create production code aligned to those artifacts.
7. Test-generation agents create:
   - unit tests for isolated business logic
   - integration tests for service and persistence boundaries
   - Testcontainers-based environment tests for external dependencies
   - performance tests for throughput, latency, and scale assumptions
   - UI tests for end-to-end user behavior
8. Review agents compare implementation and tests against the originating specifications, indexed source material, and graph relationships.

## Planned Future Sections

The next iterations of this playbook should define, in detail:

### 1. Gherkin to Architecture

- how features are grouped into implementation slices
- how technical architecture is inferred or selected
- how dependencies and data contracts are established

### 2. Gherkin to Production Code

- how coding agents receive context
- how implementation tasks are decomposed
- how code review is automated

### 3. Unit Test Workflow

- how unit tests are generated alongside code
- how coverage expectations are defined
- how failing tests are fed back into refinement loops

### 4. Integration Test Workflow

- how service boundaries are identified
- how external dependencies are modeled
- how Testcontainers environments are generated and validated

### 5. Performance Test Workflow

- how performance scenarios are derived from business features
- how SLAs or budgets are defined
- how regressions are detected and blocked

### 6. UI Test Workflow

- how user journeys are derived from Gherkin features
- how UI selectors, fixtures, and environments are managed
- how browser automation becomes part of the verification gate

## Information Needed for the Next Revision

To extend this playbook accurately, the next useful inputs are:

1. how you move from approved Gherkin into architecture and coding tasks
2. which coding agents or platforms are used after DockOck
3. the target tech stack patterns you want the playbook to standardize
4. how unit tests are expected to be authored or generated
5. how integration tests should use Testcontainers in your workflow
6. how performance tests are executed and what budgets matter
7. how UI tests are authored and run
8. the actual screenshot files if you want them embedded directly in the document