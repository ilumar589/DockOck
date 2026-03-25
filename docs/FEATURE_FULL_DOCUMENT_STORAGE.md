# Full Document Storage & MCP Retrieval

DockOck stores the complete generated output of every processing mode in MongoDB, making all artifacts available for direct retrieval via the MCP server and the built-in Chat UI.

## MongoDB Collections

| Collection | Contents | Key |
|---|---|---|
| `markdown_documents` | Full rendered Markdown knowledge-base documents | Source filename |
| `gherkin_documents` | Full `.feature` Gherkin files | Source filename |
| `source_documents` | Original parsed/extracted text from input files | Source filename |

These are **plain document collections** (no vector indexes) — they exist alongside the vector-indexed collections (`sections`, `scenarios`, `chunks`, etc.) and serve a different purpose: **direct retrieval by filename** rather than semantic search.

## For UI Users

### How It Works

When you run a processing pipeline in DockOck, the system automatically stores full documents in MongoDB. This happens transparently during:

- **Gherkin mode** — Generated `.feature` files are stored, plus the original source text
- **Markdown mode** — Generated markdown knowledge-base documents are stored, plus the original source text
- **IndexOnly mode** — The original source text from all parsed files and any previously generated Gherkin/Markdown artifacts from the session are stored

### What This Enables

1. **Cross-session persistence** — Your generated artifacts survive beyond the current UI session. The next time you open DockOck, previously generated documents can be retrieved from MongoDB even if the session JSON is lost.

2. **MCP sharing** — External AI agents (e.g. Copilot, Cursor, Claude) connected to the DockOck MCP server can retrieve your full documents, not just search-result snippets.

3. **Chat-aware context** — The Chat panel's RAG pipeline continues to use the vector-indexed collections (`sections`, `scenarios`, `chunks`) for semantic search. The full-document collections complement this by enabling exact document retrieval when an agent already knows which file it needs.

### No Action Required

Storage is fully automatic. As long as MongoDB is running and a RAG provider is configured, documents are stored every time the pipeline runs.

## For MCP Agent Users

### Available Tools

The MCP server (default port `3100`) exposes these retrieval tools alongside the existing `query_documents`, `search_documents`, and `list_documents`:

#### `get_document`

Retrieve the full generated **Markdown** knowledge-base document for a source file.

```json
{
  "method": "tools/call",
  "params": {
    "name": "get_document",
    "arguments": { "filename": "D028.docx" }
  }
}
```

**Response:**
```json
{
  "content": [{ "type": "text", "text": "# D028 — Requirements\n\n## Summary\n..." }],
  "_meta": { "document": "D028.docx", "char_count": 14200 }
}
```

#### `get_gherkin`

Retrieve the full generated **Gherkin .feature** file for a source file.

```json
{
  "method": "tools/call",
  "params": {
    "name": "get_gherkin",
    "arguments": { "filename": "D028.docx" }
  }
}
```

**Response:**
```json
{
  "content": [{ "type": "text", "text": "Feature: D028 Requirements\n  Scenario: ..." }],
  "_meta": { "document": "D028.docx", "char_count": 8500 }
}
```

#### `get_source`

Retrieve the **original parsed source text** extracted from an input document.

```json
{
  "method": "tools/call",
  "params": {
    "name": "get_source",
    "arguments": { "filename": "D028.docx" }
  }
}
```

**Response:**
```json
{
  "content": [{ "type": "text", "text": "1. Introduction\nThis document describes..." }],
  "_meta": { "document": "D028.docx", "file_type": "Word", "char_count": 32000 }
}
```

### Recommended Agent Workflow

1. **Discover** — Call `list_documents` to see all indexed files and their available formats
2. **Search** — Call `query_documents` or `search_documents` for semantic retrieval when you don't know which file contains the answer
3. **Retrieve** — Call `get_document`, `get_gherkin`, or `get_source` when you know the exact filename and want the complete content

### Error Handling

If a document hasn't been generated in the requested format (e.g. calling `get_gherkin` for a file that was only processed in Markdown mode), the tool returns error code `-32001` with a helpful message suggesting `list_documents`.

### Tool Availability by Processing Mode

| Processing mode | `get_source` | `get_document` | `get_gherkin` |
|---|---|---|---|
| Gherkin | ✅ | — | ✅ |
| Markdown | ✅ | ✅ | — |
| IndexOnly | ✅ | ✅* | ✅* |

*IndexOnly re-indexes existing session artifacts, so `get_document` / `get_gherkin` are available if the files were previously processed in those modes.
