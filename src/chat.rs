//! Chat module: RAG-powered conversational Q&A over indexed documents.
//!
//! Combines vector retrieval from MongoDB with LLM generation to answer
//! user questions about the project's documents.

use anyhow::Result;
use mongodb::bson::doc;
use mongodb::Client as MongoClient;
use rig::client::{CompletionClient, EmbeddingsClient};
use rig::completion::Message;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::rag::{EmbeddingProvider, SharedVectorIndex};

// ─────────────────────────────────────────────
// Data types
// ─────────────────────────────────────────────

/// A single message in the chat history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    pub timestamp: String,
    /// Source chunks that were used to generate this answer (assistant only).
    #[serde(default)]
    pub sources: Vec<SourceReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

/// A reference to a source chunk that contributed to an answer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceReference {
    pub document_id: String,
    pub file_name: String,
    pub score: f64,
    pub excerpt: String,
}

/// Maximum number of messages to keep in conversation history.
const MAX_HISTORY: usize = 50;

/// Maximum number of history messages to include in LLM context.
const HISTORY_CONTEXT_MESSAGES: usize = 10;

/// Top-K chunks to retrieve for chat queries.
const CHAT_TOP_K: usize = 10;

/// Maximum characters of context to inject.
const MAX_CHAT_CONTEXT_CHARS: usize = 32_000;

// ─────────────────────────────────────────────
// Chat Engine
// ─────────────────────────────────────────────

/// The chat engine manages conversation state and RAG-augmented Q&A.
#[derive(Debug)]
pub struct ChatEngine {
    pub history: Vec<ChatMessage>,
}

impl Default for ChatEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatEngine {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    /// Add a user message to history.
    pub fn add_user_message(&mut self, content: String) {
        self.history.push(ChatMessage {
            role: ChatRole::User,
            content,
            timestamp: now_timestamp(),
            sources: Vec::new(),
        });
        self.trim_history();
    }

    /// Add an assistant message with source references.
    pub fn add_assistant_message(&mut self, content: String, sources: Vec<SourceReference>) {
        self.history.push(ChatMessage {
            role: ChatRole::Assistant,
            content,
            timestamp: now_timestamp(),
            sources,
        });
        self.trim_history();
    }

    /// Clear conversation history.
    pub fn clear(&mut self) {
        self.history.clear();
    }

    fn trim_history(&mut self) {
        if self.history.len() > MAX_HISTORY {
            let drain = self.history.len() - MAX_HISTORY;
            self.history.drain(..drain);
        }
    }

    /// Build the conversation history for the LLM (last N messages).
    fn build_chat_history(&self) -> Vec<Message> {
        let start = self.history.len().saturating_sub(HISTORY_CONTEXT_MESSAGES);
        self.history[start..]
            .iter()
            .filter_map(|msg| match msg.role {
                ChatRole::User => Some(Message::user(&msg.content)),
                ChatRole::Assistant => Some(Message::assistant(&msg.content)),
                ChatRole::System => None,
            })
            .collect()
    }
}

// ─────────────────────────────────────────────
// RAG-augmented query
// ─────────────────────────────────────────────

/// System prompt for the document chat assistant.
const CHAT_PREAMBLE: &str = r#"You are a knowledgeable document assistant for a project's knowledge base.
You answer questions based on the provided document context. Your answers should be:
- Accurate and grounded in the provided document excerpts
- Clear and well-structured
- Including specific references to source documents when relevant
- Honest about uncertainty — if the context doesn't contain enough information, say so

When citing sources, mention the document name in square brackets, e.g. [D028.docx].
Do not make up information that isn't supported by the provided context."#;

/// Retrieve relevant document chunks for a query.
#[instrument(skip_all, fields(query_len = query.len()))]
pub async fn retrieve_chunks(
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    query: &str,
    cancel_token: &CancellationToken,
) -> Vec<SourceReference> {
    let collection = crate::rag::chunks_collection(mongo_client);

    let results = match provider {
        EmbeddingProvider::Ollama { client, model } => {
            retrieve_via_ollama(client, model, &collection, query, cancel_token).await
        }
        EmbeddingProvider::FastEmbed => {
            retrieve_via_fastembed(&collection, query, cancel_token).await
        }
    };

    match results {
        Ok(refs) => {
            if refs.is_empty() {
                info!("Chat RAG retrieval returned 0 chunks for query (len={})", query.len());
            } else {
                info!("Chat RAG retrieval found {} chunks", refs.len());
            }
            refs
        }
        Err(e) => {
            warn!("Chat RAG retrieval failed: {e}");
            eprintln!("[CHAT] RAG retrieval error: {e}");
            Vec::new()
        }
    }
}

async fn retrieve_via_ollama(
    client: &rig::providers::ollama::Client,
    model_name: &str,
    collection: &mongodb::Collection<mongodb::bson::Document>,
    query: &str,
    cancel_token: &CancellationToken,
) -> Result<Vec<SourceReference>> {
    use rig::vector_store::VectorStoreIndex;
    use rig_mongodb::{MongoDbVectorIndex, SearchParams};

    let model = client.embedding_model(model_name);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve_chunks(&index, query, cancel_token).await
}

async fn retrieve_via_fastembed(
    collection: &mongodb::Collection<mongodb::bson::Document>,
    query: &str,
    cancel_token: &CancellationToken,
) -> Result<Vec<SourceReference>> {
    use rig::vector_store::VectorStoreIndex;
    use rig_fastembed::FastembedModel;
    use rig_mongodb::{MongoDbVectorIndex, SearchParams};

    let fe_client = rig_fastembed::Client::new();
    let model = fe_client.embedding_model(&FastembedModel::AllMiniLML6V2Q);
    let index = MongoDbVectorIndex::new(
        collection.clone(),
        model,
        "vector_index",
        SearchParams::new(),
    )
    .await?;

    do_retrieve_chunks(&index, query, cancel_token).await
}

async fn do_retrieve_chunks<I: rig::vector_store::VectorStoreIndex>(
    index: &I,
    query: &str,
    cancel_token: &CancellationToken,
) -> Result<Vec<SourceReference>> {
    use rig::vector_store::VectorSearchRequest;

    let request = VectorSearchRequest::builder()
        .query(query)
        .samples((CHAT_TOP_K + 2) as u64)
        .build()?;

    let results: Vec<(f64, String, serde_json::Value)> = tokio::select! {
        r = index.top_n(request) => r?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Chat retrieval cancelled");
        }
    };

    let mut refs = Vec::new();
    let mut chars_used = 0usize;

    for (score, id, doc) in results {
        if refs.len() >= CHAT_TOP_K {
            break;
        }

        // Extract text from the document — field is "text" for chunks
        let text = doc.get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if text.is_empty() {
            continue;
        }
        if chars_used + text.len() > MAX_CHAT_CONTEXT_CHARS {
            break;
        }

        let file_name = id.split(':').next().unwrap_or(&id).to_string();
        let excerpt = if text.len() > 200 {
            let end = text.floor_char_boundary(200);
            format!("{}…", &text[..end])
        } else {
            text.clone()
        };

        refs.push(SourceReference {
            document_id: id,
            file_name,
            score,
            excerpt,
        });
        chars_used += text.len();
    }

    Ok(refs)
}

/// Build a context block from retrieved source references.
fn build_context_from_sources(sources: &[SourceReference]) -> String {
    if sources.is_empty() {
        return String::from("No relevant document context was found for this query.");
    }

    let mut context =
        String::from("=== Relevant Document Context ===\n\n");
    for src in sources {
        context.push_str(&format!(
            "--- [{}] (relevance: {:.2}) ---\n{}\n\n",
            src.document_id, src.score, src.excerpt
        ));
    }
    context
}

/// Execute a RAG-augmented chat query.
///
/// 1. Retrieves relevant chunks from the vector store
/// 2. Builds a prompt with context + conversation history
/// 3. Streams the LLM response
/// 4. Returns the answer and source references
#[instrument(skip_all, fields(query_len = query.len()))]
pub async fn chat_query(
    query: &str,
    history: &[ChatMessage],
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    rag_indexes: &[SharedVectorIndex],
    generator_model: &str,
    ollama_client: Option<&rig::providers::ollama::Client>,
    openai_client: Option<&rig::providers::openai::CompletionsClient>,
    cancel_token: &CancellationToken,
    on_token: impl Fn(&str),
) -> Result<(String, Vec<SourceReference>)> {
    // Step 1: Retrieve relevant chunks
    let sources = retrieve_chunks(provider, mongo_client, query, cancel_token).await;
    let context_block = build_context_from_sources(&sources);

    // Step 2: Build conversation history for the LLM
    let history_start = history.len().saturating_sub(HISTORY_CONTEXT_MESSAGES);
    let mut chat_history: Vec<Message> = Vec::new();

    // Inject retrieved context as first user message
    chat_history.push(Message::user(context_block));

    // Add recent conversation history
    for msg in &history[history_start..] {
        match msg.role {
            ChatRole::User => chat_history.push(Message::user(&msg.content)),
            ChatRole::Assistant => chat_history.push(Message::assistant(&msg.content)),
            ChatRole::System => {}
        }
    }

    // Step 3: Generate answer via LLM
    let answer = if let Some(openai) = openai_client {
        chat_via_openai(
            openai,
            generator_model,
            query,
            chat_history,
            rag_indexes,
            cancel_token,
            &on_token,
        )
        .await?
    } else if let Some(ollama) = ollama_client {
        chat_via_ollama(
            ollama,
            generator_model,
            query,
            chat_history,
            rag_indexes,
            cancel_token,
            &on_token,
        )
        .await?
    } else {
        anyhow::bail!("No LLM client available for chat");
    };

    info!("Chat query answered: {} chars, {} sources", answer.len(), sources.len());
    Ok((answer, sources))
}

async fn chat_via_ollama(
    client: &rig::providers::ollama::Client,
    model: &str,
    prompt: &str,
    history: Vec<Message>,
    rag_indexes: &[SharedVectorIndex],
    cancel_token: &CancellationToken,
    on_token: &impl Fn(&str),
) -> Result<String> {
    use rig::streaming::StreamingPrompt;

    let num_ctx = crate::llm::context_window_for_model(model);
    let mut builder = client
        .agent(model)
        .preamble(CHAT_PREAMBLE)
        .additional_params(serde_json::json!({"num_ctx": num_ctx}));
    for idx in rag_indexes {
        builder = builder.dynamic_context(8, idx.clone());
    }
    let agent = builder.build();

    let stream_fut = agent.stream_prompt(prompt).with_history(history);
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(120), stream_fut)
        .await
        .map_err(|_| anyhow::anyhow!("Chat connection timed out"))?;

    stream_response(&mut stream, cancel_token, on_token).await
}

async fn chat_via_openai(
    client: &rig::providers::openai::CompletionsClient,
    model: &str,
    prompt: &str,
    history: Vec<Message>,
    rag_indexes: &[SharedVectorIndex],
    cancel_token: &CancellationToken,
    on_token: &impl Fn(&str),
) -> Result<String> {
    use rig::streaming::StreamingPrompt;

    let mut builder = client.agent(model).preamble(CHAT_PREAMBLE);
    for idx in rag_indexes {
        builder = builder.dynamic_context(8, idx.clone());
    }
    let agent = builder.build();

    let stream_fut = agent.stream_prompt(prompt).with_history(history);
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(120), stream_fut)
        .await
        .map_err(|_| anyhow::anyhow!("Chat connection timed out"))?;

    stream_response(&mut stream, cancel_token, on_token).await
}

/// Consume a multi-turn stream produced by rig-core, accumulating the
/// assistant's textual tokens and calling `on_token` for each one.
async fn stream_response<S, R, E>(
    stream: &mut S,
    cancel_token: &CancellationToken,
    on_token: &impl Fn(&str),
) -> Result<String>
where
    S: futures::Stream<Item = Result<rig::agent::MultiTurnStreamItem<R>, E>>
        + Unpin,
    E: std::fmt::Display,
{
    use futures::StreamExt;
    use rig::agent::{MultiTurnStreamItem, Text};
    use rig::streaming::StreamedAssistantContent;

    let mut accumulated = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let chunk_timeout = std::time::Duration::from_secs(60).min(remaining);

        tokio::select! {
            chunk = tokio::time::timeout(chunk_timeout, stream.next()) => {
                match chunk {
                    Ok(Some(Ok(MultiTurnStreamItem::StreamAssistantItem(
                        StreamedAssistantContent::Text(Text { text }),
                    )))) => {
                        accumulated.push_str(&text);
                        on_token(&text);
                    }
                    Ok(Some(Ok(MultiTurnStreamItem::FinalResponse(_)))) => break,
                    Ok(Some(Err(e))) => anyhow::bail!("Chat stream error: {e}"),
                    Ok(None) => break,
                    Err(_) => anyhow::bail!("Chat stream stalled"),
                    _ => {}
                }
            }
            _ = cancel_token.cancelled() => {
                anyhow::bail!("Chat cancelled during streaming");
            }
        }
    }

    Ok(accumulated)
}

/// Search documents without LLM generation — returns raw chunk matches.
/// Used by the MCP `search_documents` tool.
#[instrument(skip_all)]
pub async fn search_documents(
    query: &str,
    top_k: usize,
    provider: &EmbeddingProvider,
    mongo_client: &MongoClient,
    cancel_token: &CancellationToken,
) -> Vec<SourceReference> {
    retrieve_chunks(provider, mongo_client, query, cancel_token).await
        .into_iter()
        .take(top_k)
        .collect()
}

/// List all distinct documents that have been indexed.
#[instrument(skip_all)]
pub async fn list_indexed_documents(
    mongo_client: &MongoClient,
) -> Result<Vec<DocumentInfo>> {
    use futures::TryStreamExt;

    let collection = crate::rag::chunks_collection(mongo_client);

    // Use aggregation to group by file name and count chunks
    let pipeline = vec![
        doc! {
            "$project": {
                "file_name": {
                    "$arrayElemAt": [
                        { "$split": ["$_id", ":"] },
                        0
                    ]
                }
            }
        },
        doc! {
            "$group": {
                "_id": "$file_name",
                "chunk_count": { "$sum": 1 }
            }
        },
        doc! {
            "$sort": { "_id": 1 }
        },
    ];

    let mut cursor = collection.aggregate(pipeline).await?;
    let mut docs = Vec::new();

    while let Some(d) = cursor.try_next().await? {
        if let (Ok(name), Ok(count)) = (d.get_str("_id"), d.get_i32("chunk_count")) {
            docs.push(DocumentInfo {
                file_name: name.to_string(),
                chunk_count: count as usize,
            });
        }
    }

    Ok(docs)
}

/// Summary info about an indexed document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentInfo {
    pub file_name: String,
    pub chunk_count: usize,
}

fn now_timestamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() % 86400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
