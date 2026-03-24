//! MCP (Model Context Protocol) server — exposes document search and Q&A as
//! tools that external AI agents can consume over HTTP+SSE.
//!
//! Protocol: JSON-RPC 2.0 messages over HTTP with Server-Sent Events (SSE).
//!
//! ## Endpoints
//!
//! - `POST /mcp` — JSON-RPC 2.0 request handler
//! - `GET  /mcp/sse` — SSE stream for server-initiated messages
//! - `GET  /health` — Health check
//!
//! ## Tools exposed
//!
//! - `query_documents` — Semantic search + LLM-generated answer
//! - `search_documents` — Raw semantic search (no LLM generation)
//! - `list_documents` — List all indexed documents with chunk counts

use std::sync::Arc;

use axum::{
    Json,
    extract::State,

    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::chat;
use crate::rag::{EmbeddingProvider, SharedVectorIndex};

/// Default port for the MCP server.
pub const DEFAULT_MCP_PORT: u16 = 3100;

// ─────────────────────────────────────────────
// JSON-RPC types
// ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

// ─────────────────────────────────────────────
// MCP protocol types
// ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct McpToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

// ─────────────────────────────────────────────
// Shared state
// ─────────────────────────────────────────────

/// Shared state for the MCP server, holding references to the RAG infrastructure.
pub struct McpState {
    pub embedding_provider: EmbeddingProvider,
    pub mongo_client: mongodb::Client,
    pub rag_indexes: Vec<SharedVectorIndex>,
    pub generator_model: String,
    pub ollama_client: Option<rig::providers::ollama::Client>,
    pub openai_client: Option<rig::providers::openai::CompletionsClient>,
    pub cancel_token: CancellationToken,
    pub sse_tx: broadcast::Sender<String>,
}

// ─────────────────────────────────────────────
// Server lifecycle
// ─────────────────────────────────────────────

/// Start the MCP server on the given port.
///
/// Returns a `CancellationToken` that can be used to shut down the server.
pub async fn start_server(
    state: Arc<McpState>,
    port: u16,
) -> CancellationToken {
    let shutdown_token = CancellationToken::new();
    let token = shutdown_token.clone();

    let app = Router::new()
        .route("/mcp", post(handle_rpc))
        .route("/mcp/sse", get(handle_sse))
        .route("/health", get(handle_health))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await {
            Ok(l) => l,
            Err(e) => {
                warn!("MCP server failed to bind to port {port}: {e}");
                return;
            }
        };
        info!("MCP server listening on port {port}");

        axum::serve(listener, app)
            .with_graceful_shutdown(token.cancelled_owned())
            .await
            .unwrap_or_else(|e| warn!("MCP server error: {e}"));

        info!("MCP server shut down");
    });

    shutdown_token
}

// ─────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────

async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "dockock-mcp",
        "protocol": "mcp/1.0"
    }))
}

async fn handle_sse(
    State(state): State<Arc<McpState>>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut rx = state.sse_tx.subscribe();
    let stream = async_stream::stream! {
        // Send initial endpoint info
        let endpoint_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        yield Ok(Event::default().data(endpoint_msg.to_string()));

        loop {
            match rx.recv().await {
                Ok(msg) => {
                    yield Ok(Event::default().data(msg));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("MCP SSE client lagged by {n} messages");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream)
}

async fn handle_rpc(
    State(state): State<Arc<McpState>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if request.jsonrpc != "2.0" {
        return Json(JsonRpcResponse::error(
            request.id,
            -32600,
            "Invalid JSON-RPC version".to_string(),
        ));
    }

    let response = match request.method.as_str() {
        "initialize" => handle_initialize(request.id),
        "tools/list" => handle_tools_list(request.id),
        "tools/call" => handle_tools_call(request.id, request.params, &state).await,
        "ping" => JsonRpcResponse::success(request.id, serde_json::json!({})),
        _ => JsonRpcResponse::error(
            request.id,
            -32601,
            format!("Method not found: {}", request.method),
        ),
    };

    // Broadcast response over SSE for clients using that transport
    if response.result.is_some() {
        let msg = serde_json::to_string(&response).unwrap_or_default();
        let _ = state.sse_tx.send(msg);
    }

    Json(response)
}

fn handle_initialize(id: Option<serde_json::Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "dockock-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Option<serde_json::Value>) -> JsonRpcResponse {
    let tools = vec![
        McpToolDefinition {
            name: "query_documents".to_string(),
            description: "Search the indexed document knowledge base and generate an AI answer based on the relevant context. Returns a natural language answer with source references.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The natural language question to ask about the documents"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of source chunks to retrieve (default: 10)",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        McpToolDefinition {
            name: "search_documents".to_string(),
            description: "Perform a semantic search over indexed documents and return matching text chunks with relevance scores. No LLM generation — returns raw search results.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query text"
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 10)",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        },
        McpToolDefinition {
            name: "list_documents".to_string(),
            description: "List all documents that have been indexed in the knowledge base, with their chunk counts.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "description": "Output format: 'summary' (default) or 'detailed'",
                        "enum": ["summary", "detailed"],
                        "default": "summary"
                    }
                },
                "required": []
            }),
        },
    ];

    JsonRpcResponse::success(id, serde_json::json!({ "tools": tools }))
}

async fn handle_tools_call(
    id: Option<serde_json::Value>,
    params: serde_json::Value,
    state: &McpState,
) -> JsonRpcResponse {
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    match tool_name {
        "query_documents" => tool_query_documents(id, arguments, state).await,
        "search_documents" => tool_search_documents(id, arguments, state).await,
        "list_documents" => tool_list_documents(id, state).await,
        _ => JsonRpcResponse::error(
            id,
            -32602,
            format!("Unknown tool: {tool_name}"),
        ),
    }
}

// ─────────────────────────────────────────────
// Tool implementations
// ─────────────────────────────────────────────

async fn tool_query_documents(
    id: Option<serde_json::Value>,
    args: serde_json::Value,
    state: &McpState,
) -> JsonRpcResponse {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.to_string(),
        _ => {
            return JsonRpcResponse::error(id, -32602, "Missing or empty 'query' parameter".to_string());
        }
    };

    let (answer, sources) = match chat::chat_query(
        &query,
        &[],  // No conversation history for MCP tool calls
        &state.embedding_provider,
        &state.mongo_client,
        &state.rag_indexes,
        &state.generator_model,
        state.ollama_client.as_ref(),
        state.openai_client.as_ref(),
        &state.cancel_token,
        |_| {},  // No streaming callback for MCP
    )
    .await
    {
        Ok((answer, sources)) => (answer, sources),
        Err(e) => {
            return JsonRpcResponse::error(id, -32000, format!("Query failed: {e}"));
        }
    };

    let source_list: Vec<serde_json::Value> = sources
        .iter()
        .map(|s| {
            serde_json::json!({
                "document": s.file_name,
                "chunk_id": s.document_id,
                "relevance": s.score,
                "excerpt": s.excerpt,
            })
        })
        .collect();

    let result_text = format!(
        "{}\n\n---\nSources: {}",
        answer,
        sources
            .iter()
            .map(|s| format!("[{}]", s.file_name))
            .collect::<Vec<_>>()
            .join(", ")
    );

    JsonRpcResponse::success(
        id,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": result_text
            }],
            "_meta": {
                "sources": source_list
            }
        }),
    )
}

async fn tool_search_documents(
    id: Option<serde_json::Value>,
    args: serde_json::Value,
    state: &McpState,
) -> JsonRpcResponse {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.to_string(),
        _ => {
            return JsonRpcResponse::error(id, -32602, "Missing or empty 'query' parameter".to_string());
        }
    };

    let top_k = args
        .get("top_k")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    let results = chat::search_documents(
        &query,
        top_k,
        &state.embedding_provider,
        &state.mongo_client,
        &state.cancel_token,
    )
    .await;

    let mut text = String::from("Search results:\n\n");
    for (i, r) in results.iter().enumerate() {
        text.push_str(&format!(
            "{}. [{}] (score: {:.2})\n{}\n\n",
            i + 1,
            r.file_name,
            r.score,
            r.excerpt
        ));
    }

    let results_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "document": r.file_name,
                "chunk_id": r.document_id,
                "relevance": r.score,
                "excerpt": r.excerpt,
            })
        })
        .collect();

    JsonRpcResponse::success(
        id,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "_meta": {
                "results": results_json
            }
        }),
    )
}

async fn tool_list_documents(
    id: Option<serde_json::Value>,
    state: &McpState,
) -> JsonRpcResponse {
    match chat::list_indexed_documents(&state.mongo_client).await {
        Ok(docs) => {
            let mut text = format!("Indexed documents ({} total):\n\n", docs.len());
            for d in &docs {
                let stem = d.file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&d.file_name);
                let variants = format!("{}.feature, {}.md", stem, stem);
                text.push_str(&format!(
                    "- {} ({} chunks) — available as: {}\n",
                    d.file_name, d.chunk_count, variants,
                ));
            }

            let docs_json: Vec<serde_json::Value> = docs
                .iter()
                .map(|d| {
                    let stem = d.file_name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&d.file_name);
                    serde_json::json!({
                        "file_name": d.file_name,
                        "chunk_count": d.chunk_count,
                        "available_as": [
                            format!("{}.feature", stem),
                            format!("{}.md", stem),
                        ],
                    })
                })
                .collect();

            JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": text
                    }],
                    "_meta": {
                        "documents": docs_json
                    }
                }),
            )
        }
        Err(e) => JsonRpcResponse::error(id, -32000, format!("Failed to list documents: {e}")),
    }
}
