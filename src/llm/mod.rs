//! LLM integration using `rig-core` with a local Ollama instance.
//!
//! The module wraps the rig-core `Agent` so the rest of the application only
//! needs to call the high-level [`generate_gherkin`] function.
//!
//! ## Requirements
//! A running Ollama instance is expected at `http://localhost:11434`.
//! Start one with Docker Compose (see `docker-compose.yml`) or directly with
//! `ollama serve`.

use anyhow::{Context, Result};
use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama;
use tokio;

use crate::context::ProjectContext;

/// Default model used when none is specified.
pub const DEFAULT_MODEL: &str = "llama3.2";

/// System preamble that instructs the model to generate Gherkin output.
const SYSTEM_PREAMBLE: &str = r#"You are an expert business analyst and technical writer.
Your task is to read extracted document content and produce well-structured Gherkin
Feature documentation that can be used by OpenSpec to generate project implementations.

Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Create meaningful Scenarios that cover the key behaviours described in the document.
3. Use concrete, business-readable language in steps.
4. Where cross-file context is provided, reference other components or actors correctly.
5. Do not add explanatory prose outside the Gherkin block.
6. Always end with a blank line after the last Scenario."#;

/// Generate a Gherkin feature document from the given `file_content` string.
///
/// `context` carries information extracted from all *other* files that have
/// already been processed so that the model can produce consistent output
/// across the whole project.
///
/// `model_name` overrides the default Ollama model if provided.
pub async fn generate_gherkin(
    file_name: &str,
    file_type: &str,
    file_content: &str,
    context: &ProjectContext,
    model_name: Option<&str>,
) -> Result<String> {
    let client = ollama::Client::new(Nothing)
        .context("Failed to create Ollama client. Is Ollama running on port 11434?")?;

    let model = model_name.unwrap_or(DEFAULT_MODEL);

    let agent = client
        .agent(model)
        .preamble(SYSTEM_PREAMBLE)
        .build();

    let context_summary = context.build_summary();

    let prompt = format!(
        r#"Convert the following {file_type} document content into a Gherkin Feature file.

Document: {file_name}

{context_section}
=== Document Content ===
{file_content}

Generate the Gherkin Feature below:"#,
        file_type = file_type,
        file_name = file_name,
        context_section = if context_summary.contains("No prior files") {
            String::new()
        } else {
            format!("{}\n", context_summary)
        },
        file_content = file_content,
    );

    let response = agent
        .prompt(prompt.as_str())
        .await
        .context("Failed to get response from Ollama LLM")?;

    Ok(response)
}

/// Check whether the Ollama server is reachable.
///
/// Returns `Ok(())` on success or an error with a helpful message.
pub async fn check_ollama_connection() -> Result<()> {
    use std::net::TcpStream;
    use std::time::Duration;

    tokio::task::spawn_blocking(|| {
        TcpStream::connect_timeout(
            &"127.0.0.1:11434"
                .parse()
                .expect("hardcoded address is always valid"),
            Duration::from_secs(3),
        )
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(
            "Cannot reach Ollama at http://localhost:11434: {}. \
             Make sure Ollama is running (see docker-compose.yml).",
            e
        ))
    })
    .await
    .context("Failed to spawn connection check")?
}
