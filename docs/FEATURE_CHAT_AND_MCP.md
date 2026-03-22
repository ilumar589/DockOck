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
