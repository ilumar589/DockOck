# Feature: Document Chat & MCP Tool Server

## Overview

Add a RAG-powered chat interface to DockOck that lets users query their indexed
documents in natural language, and expose the same capability as an MCP
(Model Context Protocol) tool server so external AI agents can consume the
knowledge base.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  DockOck Application                                            │
│                                                                 │
│  ┌──────────┐   ┌──────────────┐   ┌────────────────────────┐  │
│  │ egui UI  │──▶│ Chat Module  │──▶│ RAG (rag.rs)           │  │
│  │ Chat Tab │   │ (chat.rs)    │   │ MongoDB Vector Search  │  │
│  └──────────┘   └──────┬───────┘   └────────────────────────┘  │
│                         │                                       │
│                         ▼                                       │
│                  ┌──────────────┐                                │
│                  │ LLM Provider │  (Ollama / Custom API)        │
│                  └──────────────┘                                │
│                                                                 │
│  ┌──────────────────────┐                                       │
│  │ MCP Server (mcp.rs)  │ ◀── HTTP+SSE on port 3100            │
│  │ - query_documents    │     External agents connect here      │
│  │ - list_documents     │                                       │
│  │ - get_document_chunk │                                       │
│  └──────────────────────┘                                       │
└─────────────────────────────────────────────────────────────────┘
```

## Implementation Plan

### Phase 1: Chat Module (`src/chat.rs`)

**Purpose**: Core query engine that combines RAG retrieval with LLM generation.

- `ChatMessage` struct (role, content, timestamp)
- `ChatEngine` struct holding references to RAG provider, MongoDB, LLM client
- `query()` method:
  1. Embed the user's question via the active embedding provider
  2. Retrieve top-K relevant chunks from MongoDB vector store
  3. Build a prompt with retrieved context + conversation history
  4. Send to LLM and stream the response
  5. Return the answer with source citations

### Phase 2: Chat UI Panel

**Purpose**: Add an interactive chat tab to the egui interface.

- New "Chat" tab alongside existing output panel
- Message history display with user/assistant bubbles
- Text input field with send button
- Source citations shown as collapsible references
- Loading indicator during LLM streaming
- Chat state persisted in `DockOckApp`

### Phase 3: MCP Tool Server (`src/mcp.rs`)

**Purpose**: Expose document querying as MCP tools over HTTP+SSE.

Tools exposed:
1. **`query_documents`** — Semantic search + LLM answer generation
   - Input: `{ query: string, top_k?: number }`
   - Returns: Answer text with source references
2. **`search_documents`** — Raw semantic search (no LLM)
   - Input: `{ query: string, top_k?: number }`
   - Returns: Array of matching chunks with scores
3. **`list_documents`** — List all indexed documents
   - Returns: Array of document names and chunk counts

The MCP server runs on a configurable port (default 3100) using the
JSON-RPC 2.0 over HTTP with SSE transport as per the MCP specification.

### Phase 4: Integration

- Register `chat` and `mcp` modules in `main.rs`
- Start MCP server when documents are indexed
- Add MCP server toggle and port configuration to UI
- Document the MCP endpoint for agent consumption

## Dependencies Added

- `axum` — HTTP server for MCP transport
- `tower` — Middleware for the HTTP server
- `tower-http` — CORS support for MCP
- `uuid` — Session IDs for MCP

## Configuration

| Setting       | Default | Description                        |
|---------------|---------|------------------------------------|
| MCP Port      | 3100    | Port for the MCP HTTP+SSE server   |
| MCP Enabled   | false   | Whether to start the MCP server    |
| Chat History  | 50      | Max messages kept in chat history   |

---

## MCP API Reference

The MCP server speaks **JSON-RPC 2.0** over HTTP. All tool calls go through
`POST /mcp`. An SSE stream is available for server-initiated messages.

> **Prerequisites:** Documents must be indexed first (run "Index Documents" in
> the UI with RAG enabled) and the MCP server must be started via the 🔌 MCP
> button in the bottom bar.

### Endpoints

| Method | Path        | Description                              |
|--------|-------------|------------------------------------------|
| GET    | `/health`   | Health check — returns service status    |
| POST   | `/mcp`      | JSON-RPC 2.0 request handler             |
| GET    | `/mcp/sse`  | SSE stream for server-initiated messages |

### JSON-RPC Methods

| Method          | Description                                    |
|-----------------|------------------------------------------------|
| `initialize`    | Handshake — returns server capabilities        |
| `tools/list`    | List available tools with input schemas        |
| `tools/call`    | Invoke a tool by name with arguments           |
| `ping`          | Connectivity check                             |

### Tools

#### `query_documents`
Semantic search + LLM-generated answer with source references.

| Parameter | Type    | Required | Default | Description                      |
|-----------|---------|----------|---------|----------------------------------|
| `query`   | string  | yes      | —       | Natural language question        |
| `top_k`   | integer | no       | 10      | Max source chunks to retrieve    |

**Response:** `content[].text` contains the answer. `_meta.sources[]` has structured source data (document name, chunk ID, relevance score, excerpt).

#### `search_documents`
Raw semantic search — returns matching chunks without LLM generation.

| Parameter | Type    | Required | Default | Description                      |
|-----------|---------|----------|---------|----------------------------------|
| `query`   | string  | yes      | —       | Search query text                |
| `top_k`   | integer | no       | 10      | Max results to return            |

**Response:** `content[].text` contains formatted results. `_meta.results[]` has structured data (document, chunk_id, relevance, excerpt).

#### `list_documents`
List all indexed documents with chunk counts. No parameters.

**Response:** `content[].text` contains a formatted list. `_meta.documents[]` has structured data (file_name, chunk_count).

### curl Examples

```bash
# Health check
curl http://localhost:3100/health

# Initialize (handshake)
curl -X POST http://localhost:3100/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'

# List available tools
curl -X POST http://localhost:3100/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# Query documents (semantic search + LLM answer)
curl -X POST http://localhost:3100/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"query_documents","arguments":{"query":"How does meter creation work?","top_k":5}}}'

# Search documents (raw vector search, no LLM)
curl -X POST http://localhost:3100/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_documents","arguments":{"query":"inspection status flow","top_k":10}}}'

# List indexed documents
curl -X POST http://localhost:3100/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_documents","arguments":{}}}'

# SSE stream (live server events)
curl -N http://localhost:3100/mcp/sse
```

### Postman

A ready-to-import Postman collection is available at
[`postman/DockOck_MCP.postman_collection.json`](../postman/DockOck_MCP.postman_collection.json).
Import it into Postman via **File → Import** and set the `base_url` variable
if your server is not on `localhost:3100`.
