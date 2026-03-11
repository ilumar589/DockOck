//! Multi-agent LLM integration using `rig-core` with local Ollama instances.
//!
//! ## Architecture
//!
//! The pipeline has three configurable modes:
//!
//! | Mode     | Steps                                | LLM calls |
//! |----------|--------------------------------------|-----------|
//! | Fast     | Preprocess → Generate                | 1         |
//! | Standard | Preprocess → Generate → Review       | 2         |
//! | Full     | Extract(LLM) → Generate → Review     | 3         |
//!
//! The **Preprocess** step is a zero-cost Rust text truncation/structuring pass
//! that replaces the slow LLM extractor in Fast and Standard modes.
//!
//! Files are parsed in parallel, then processed through the agent pipeline
//! concurrently (up to `MAX_CONCURRENT` at a time).

use anyhow::{Context, Result};
use futures::StreamExt;
use rig::agent::{MultiTurnStreamItem, Text};
use rig::client::{CompletionClient, Nothing};
use rig::providers::ollama;
use rig::streaming::{StreamedAssistantContent, StreamingPrompt};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::context::ProjectContext;

/// Default model used when none is specified.
pub const DEFAULT_MODEL: &str = "llama3.2";

/// Maximum number of files processed through the LLM pipeline simultaneously.
pub const MAX_CONCURRENT: usize = 3;

/// Maximum number of characters to send to the LLM in a single prompt.
/// Text beyond this limit is truncated with a note.
/// 12 000 chars ≈ 3 000 tokens — enough for a 14-page service document
/// while staying well within typical context windows.
const MAX_INPUT_CHARS: usize = 12_000;

/// Pipeline mode — controls which LLM stages are executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineMode {
    /// Preprocess (fast) → Generate.  1 LLM call.
    Fast,
    /// Preprocess (fast) → Generate → Review.  2 LLM calls.
    Standard,
    /// Extract (LLM) → Generate → Review.  3 LLM calls.
    Full,
}

impl Default for PipelineMode {
    fn default() -> Self {
        Self::Fast
    }
}

impl std::fmt::Display for PipelineMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => write!(f, "Fast (1 LLM call)"),
            Self::Standard => write!(f, "Standard (2 LLM calls)"),
            Self::Full => write!(f, "Full (3 LLM calls)"),
        }
    }
}

impl PipelineMode {
    pub const ALL: [PipelineMode; 3] = [Self::Fast, Self::Standard, Self::Full];
}

/// Ollama instance definitions.
#[derive(Debug, Clone)]
pub struct OllamaEndpoint {
    pub name: &'static str,
    pub url: &'static str,
    pub port: u16,
}

pub const ENDPOINT_GENERATOR: OllamaEndpoint = OllamaEndpoint {
    name: "Generator",
    url: "http://localhost:11434",
    port: 11434,
};

pub const ENDPOINT_EXTRACTOR: OllamaEndpoint = OllamaEndpoint {
    name: "Extractor",
    url: "http://localhost:11435",
    port: 11435,
};

pub const ENDPOINT_REVIEWER: OllamaEndpoint = OllamaEndpoint {
    name: "Reviewer",
    url: "http://localhost:11436",
    port: 11436,
};

// ─────────────────────────────────────────────
// Agent preambles
// ─────────────────────────────────────────────

const EXTRACTOR_PREAMBLE: &str = r#"You are an expert document analyst.
Your task is to read raw extracted document content and produce a concise structured summary.

Rules:
1. Identify the key actors, systems, data entities, and processes described.
2. List preconditions and postconditions for each process.
3. Capture business rules and validation logic.
4. Output in a structured format with sections: ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES.
5. Be concise — no more than 300 words.
6. Do not add conversational prose."#;

const GENERATOR_PREAMBLE: &str = r#"You are an expert business analyst and technical writer.
Your task is to read a structured document summary and produce well-structured Gherkin
Feature documentation that can be used by OpenSpec to generate project implementations.

Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Create meaningful Scenarios that cover the key behaviours described.
3. Use concrete, business-readable language in steps.
4. Where cross-file context is provided, reference other components or actors correctly.
5. Do not add explanatory prose outside the Gherkin block.
6. Always end with a blank line after the last Scenario."#;

const REVIEWER_PREAMBLE: &str = r#"You are a Gherkin quality reviewer.
Your task is to review and improve a Gherkin Feature document.

Rules:
1. Fix any Gherkin syntax errors (Feature/Scenario/Given/When/Then/And/But).
2. Ensure scenarios are complete (have at least Given, When, Then).
3. Improve step clarity and business readability where needed.
4. Remove duplicate scenarios.
5. Output ONLY the corrected Gherkin — no explanations.
6. If the input is already good, return it unchanged."#;

// ─────────────────────────────────────────────
// Client creation
// ─────────────────────────────────────────────

fn create_client_for(endpoint: &OllamaEndpoint) -> Result<ollama::Client> {
    ollama::Client::builder()
        .api_key(Nothing)
        .base_url(endpoint.url)
        .build()
        .with_context(|| format!(
            "Failed to create Ollama client for {} at {}",
            endpoint.name, endpoint.url
        ))
}

/// The orchestrator that owns clients for all reachable Ollama instances.
pub struct AgentOrchestrator {
    extractor_client: Option<ollama::Client>,
    generator_client: ollama::Client,
    reviewer_client: Option<ollama::Client>,
    model: String,
    pub semaphore: Arc<Semaphore>,
    pub mode: PipelineMode,
}

/// Result of checking which Ollama endpoints are reachable.
#[derive(Debug, Clone)]
pub struct EndpointStatus {
    pub name: &'static str,
    pub url: &'static str,
    pub reachable: bool,
}

impl AgentOrchestrator {
    /// Create the orchestrator, probing all endpoints.
    /// At minimum the generator (port 11434) must be reachable.
    pub async fn new(model: &str, mode: PipelineMode) -> Result<(Self, Vec<EndpointStatus>)> {
        let mut statuses = Vec::new();

        // Check each endpoint
        let gen_ok = check_endpoint(&ENDPOINT_GENERATOR).await;
        statuses.push(EndpointStatus {
            name: ENDPOINT_GENERATOR.name,
            url: ENDPOINT_GENERATOR.url,
            reachable: gen_ok,
        });

        let ext_ok = check_endpoint(&ENDPOINT_EXTRACTOR).await;
        statuses.push(EndpointStatus {
            name: ENDPOINT_EXTRACTOR.name,
            url: ENDPOINT_EXTRACTOR.url,
            reachable: ext_ok,
        });

        let rev_ok = check_endpoint(&ENDPOINT_REVIEWER).await;
        statuses.push(EndpointStatus {
            name: ENDPOINT_REVIEWER.name,
            url: ENDPOINT_REVIEWER.url,
            reachable: rev_ok,
        });

        if !gen_ok {
            anyhow::bail!(
                "Generator Ollama instance at {} is not reachable. \
                 At minimum this instance must be running.",
                ENDPOINT_GENERATOR.url
            );
        }

        let generator_client = create_client_for(&ENDPOINT_GENERATOR)?;

        let extractor_client = if ext_ok {
            info!("Extractor agent available at {}", ENDPOINT_EXTRACTOR.url);
            Some(create_client_for(&ENDPOINT_EXTRACTOR)?)
        } else {
            warn!("Extractor instance not available — generator will handle extraction");
            None
        };

        let reviewer_client = if rev_ok {
            info!("Reviewer agent available at {}", ENDPOINT_REVIEWER.url);
            Some(create_client_for(&ENDPOINT_REVIEWER)?)
        } else {
            warn!("Reviewer instance not available — skipping review step");
            None
        };

        let active_count = 1 + ext_ok as usize + rev_ok as usize;
        let concurrency = MAX_CONCURRENT.max(active_count);

        Ok((
            Self {
                extractor_client,
                generator_client,
                reviewer_client,
                model: model.to_string(),
                semaphore: Arc::new(Semaphore::new(concurrency)),
                mode,
            },
            statuses,
        ))
    }

    /// Run the pipeline for one file. Stages depend on `self.mode`.
    pub async fn process_file(
        &self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
        context: &ProjectContext,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        // ── Step 1: Prepare input for the generator ──
        let summary = if self.mode == PipelineMode::Full {
            // Full mode: use LLM extractor
            let _ = status_tx.send(format!(
                "🔍 [Extractor] Analysing {}…", file_name
            ));
            self.extract(file_name, file_type, raw_text, status_tx).await
                .unwrap_or_else(|e| {
                    warn!("Extraction failed for {}: {} — falling back to preprocessor", file_name, e);
                    preprocess_text(raw_text, file_name, file_type)
                })
        } else {
            // Fast / Standard: instant Rust preprocessor (no LLM)
            let _ = status_tx.send(format!(
                "⚡ [Preprocess] Structuring {}…", file_name
            ));
            preprocess_text(raw_text, file_name, file_type)
        };

        // ── Step 2: Generate Gherkin ──
        let _ = status_tx.send(format!(
            "⚙ [Generator] Creating Gherkin for {}…", file_name
        ));

        let context_summary = context.build_summary();
        let gherkin = self.generate(file_name, &summary, &context_summary, status_tx).await?;

        // ── Step 3: Review / refine (Standard and Full modes only) ──
        let do_review = self.mode != PipelineMode::Fast && self.reviewer_client.is_some();
        if do_review {
            let _ = status_tx.send(format!(
                "✅ [Reviewer] Validating Gherkin for {}…", file_name
            ));
            match self.review(file_name, &gherkin, status_tx).await {
                Ok(refined) => Ok(refined),
                Err(e) => {
                    warn!("Review failed for {}: {} — using unreviewed output", file_name, e);
                    Ok(gherkin)
                }
            }
        } else {
            Ok(gherkin)
        }
    }

    async fn extract(
        &self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let client = self.extractor_client.as_ref().unwrap_or(&self.generator_client);
        let agent = client.agent(&self.model).preamble(EXTRACTOR_PREAMBLE).build();

        let prompt = format!(
            "Analyse the following {file_type} document and produce a structured summary.\n\n\
             Document: {file_name}\n\n\
             === Document Content ===\n\
             {raw_text}\n\n\
             Structured summary:",
        );

        stream_with_progress(
            &agent,
            &prompt,
            "Extractor",
            file_name,
            status_tx,
            std::time::Duration::from_secs(120),
        )
        .await
    }

    async fn generate(
        &self,
        file_name: &str,
        summary: &str,
        context_summary: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let agent = self.generator_client.agent(&self.model).preamble(GENERATOR_PREAMBLE).build();

        let context_section = if context_summary.contains("No prior files") {
            String::new()
        } else {
            format!("{}\n", context_summary)
        };

        let prompt = format!(
            "Convert the following structured document summary into a Gherkin Feature file.\n\n\
             Document: {file_name}\n\n\
             {context_section}\
             === Structured Summary ===\n\
             {summary}\n\n\
             Generate the Gherkin Feature below:",
        );

        stream_with_progress(
            &agent,
            &prompt,
            "Generator",
            file_name,
            status_tx,
            std::time::Duration::from_secs(180),
        )
        .await
    }

    async fn review(
        &self,
        file_name: &str,
        gherkin: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let client = self.reviewer_client.as_ref().unwrap_or(&self.generator_client);
        let agent = client.agent(&self.model).preamble(REVIEWER_PREAMBLE).build();

        let prompt = format!(
            "Review and correct the following Gherkin Feature:\n\n\
             {gherkin}\n\n\
             Corrected Gherkin:",
        );

        stream_with_progress(
            &agent,
            &prompt,
            "Reviewer",
            file_name,
            status_tx,
            std::time::Duration::from_secs(120),
        )
        .await
    }
}

// ─────────────────────────────────────────────
// Fast text preprocessor (no LLM)
// ─────────────────────────────────────────────

/// Instantly structure and truncate raw document text for the generator prompt.
/// Replaces the slow LLM extractor in Fast and Standard modes.
fn preprocess_text(raw_text: &str, file_name: &str, file_type: &str) -> String {
    let lines: Vec<&str> = raw_text.lines().collect();
    let total_lines = lines.len();

    // Collect non-empty lines, trim whitespace
    let meaningful: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    // Build a structured output
    let mut result = format!(
        "Document: {file_name}\nType: {file_type}\nTotal lines: {total_lines}\n\n"
    );

    // Take content up to the char limit
    let mut chars_used = result.len();
    for line in &meaningful {
        if chars_used + line.len() + 1 > MAX_INPUT_CHARS {
            result.push_str("\n[… content truncated …]\n");
            break;
        }
        result.push_str(line);
        result.push('\n');
        chars_used += line.len() + 1;
    }

    result
}

// ─────────────────────────────────────────────
// Streaming helper
// ─────────────────────────────────────────────

/// Stream a prompt to an agent, accumulating the full response text and sending
/// periodic progress updates via `status_tx`.
async fn stream_with_progress<M, P>(
    agent: &rig::agent::Agent<M, P>,
    prompt: &str,
    stage_name: &str,
    file_name: &str,
    status_tx: &std::sync::mpsc::Sender<String>,
    timeout: std::time::Duration,
) -> Result<String>
where
    M: rig::completion::CompletionModel + 'static,
    M::StreamingResponse: rig::completion::GetTokenUsage,
    P: rig::agent::PromptHook<M> + 'static,
{
    let mut stream = tokio::time::timeout(timeout, agent.stream_prompt(prompt))
        .await
        .with_context(|| format!("{stage_name} timed out after {}s", timeout.as_secs()))?;

    let mut accumulated = String::new();
    let mut token_count: usize = 0;

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(Text { text }),
            )) => {
                accumulated.push_str(&text);
                token_count += 1;
                // Send progress every 20 tokens
                if token_count % 20 == 0 {
                    let _ = status_tx.send(format!(
                        "🔄 [{stage_name}] {file_name}: {token_count} tokens…"
                    ));
                }
            }
            Ok(MultiTurnStreamItem::FinalResponse(_)) => {
                break;
            }
            Err(e) => {
                eprintln!("[{stage_name} STREAM ERROR] {file_name}: {e:?}");
                anyhow::bail!("{stage_name} stream error for {file_name}: {e}");
            }
            _ => {}
        }
    }

    if accumulated.is_empty() {
        anyhow::bail!("{stage_name} returned empty response for {file_name}");
    }

    let _ = status_tx.send(format!(
        "✓ [{stage_name}] {file_name}: done ({token_count} tokens, {} chars)",
        accumulated.len()
    ));

    Ok(accumulated)
}

// ─────────────────────────────────────────────
// Connection checks
// ─────────────────────────────────────────────

async fn check_endpoint(endpoint: &OllamaEndpoint) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;

    let port = endpoint.port;
    tokio::task::spawn_blocking(move || {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port)
                .parse()
                .expect("valid address"),
            Duration::from_secs(2),
        )
        .is_ok()
    })
    .await
    .unwrap_or(false)
}

/// Check whether the primary Ollama server is reachable.
pub async fn check_ollama_connection() -> Result<()> {
    if check_endpoint(&ENDPOINT_GENERATOR).await {
        Ok(())
    } else {
        anyhow::bail!(
            "Cannot reach Ollama at {}. Make sure Ollama is running (see docker-compose.yml).",
            ENDPOINT_GENERATOR.url
        )
    }
}
