use anyhow::{Context, Result};
use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama;
use std::sync::Arc;
use tokio::sync::Semaphore;

const EXTRACTOR_PREAMBLE: &str = r#"You are an expert document analyst.
Your task is to read raw extracted document content and produce a concise structured summary.
Rules:
1. Identify the key actors, systems, data entities, and processes described.
2. List preconditions and postconditions for each process.
3. Capture business rules and validation logic.
4. Output in a structured format with sections: ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES.
5. Be concise — no more than 300 words.
6. Do not add conversational prose."#;

const GENERATOR_PREAMBLE: &str = r#"You are an expert business analyst.
Your task is to read a structured document summary and produce well-structured Gherkin Feature documentation.
Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Create meaningful Scenarios that cover the key behaviours described.
3. Use concrete, business-readable language in steps.
4. Do not add explanatory prose outside the Gherkin block.
5. Always end with a blank line after the last Scenario."#;

fn create_client(url: &str) -> Result<ollama::Client> {
    ollama::Client::builder()
        .api_key(Nothing)
        .base_url(url)
        .build()
        .with_context(|| format!("Failed to create client for {}", url))
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Full pipeline test ===\n");

    let extractor_client = create_client("http://localhost:11435")?;
    let generator_client = create_client("http://localhost:11434")?;

    let raw_text = "This document describes a user login process. \
        The user enters their username and password on the login page. \
        The system validates the credentials against the database. \
        If valid, the user is redirected to the dashboard. \
        If invalid, an error message is shown. \
        After three failed attempts the account is locked for 15 minutes.";

    // Step 1: Extraction
    println!("--- Step 1: Extraction ---");
    let agent = extractor_client
        .agent("llama3.2")
        .preamble(EXTRACTOR_PREAMBLE)
        .build();

    let prompt = format!(
        "Analyse the following word document and produce a structured summary.\n\n\
         Document: test_doc.docx\n\n\
         === Document Content ===\n\
         {raw_text}\n\n\
         Structured summary:"
    );

    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        agent.prompt(prompt.as_str()),
    )
    .await
    {
        Ok(Ok(summary)) => {
            println!("Extraction OK ({} chars)\n{}\n", summary.len(), &summary[..summary.len().min(200)]);

            // Step 2: Generation
            println!("--- Step 2: Generation ---");
            let gen_agent = generator_client
                .agent("llama3.2")
                .preamble(GENERATOR_PREAMBLE)
                .build();

            let gen_prompt = format!(
                "Convert the following structured document summary into a Gherkin Feature file.\n\n\
                 Document: test_doc.docx\n\n\
                 === Structured Summary ===\n\
                 {summary}\n\n\
                 Generate the Gherkin Feature below:"
            );

            match tokio::time::timeout(
                std::time::Duration::from_secs(180),
                gen_agent.prompt(gen_prompt.as_str()),
            )
            .await
            {
                Ok(Ok(gherkin)) => {
                    println!("Generation OK:\n{}\n", gherkin);
                }
                Ok(Err(e)) => println!("Generation FAILED: {:?}", e),
                Err(_) => println!("Generation TIMED OUT"),
            }
        }
        Ok(Err(e)) => println!("Extraction FAILED: {:?}", e),
        Err(_) => println!("Extraction TIMED OUT"),
    }

    println!("\n=== Done ===");
    Ok(())
}
