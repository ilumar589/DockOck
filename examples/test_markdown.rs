use anyhow::{Context, Result};
use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama;

const MARKDOWN_EXTRACTOR_PREAMBLE: &str = r#"You are a senior technical documentation architect.
Your task is to extract a structured knowledge inventory from the given document.
Rules:
1. Identify database tables, columns, types and constraints.
2. Identify API endpoints, methods and payloads.
3. Identify architecture layers and component relationships.
4. Capture business rules and validation logic.
5. Output sections: DATABASE, API_ENDPOINTS, ARCHITECTURE, BUSINESS_RULES, DATA_ENTITIES.
6. Be concise — no more than 400 words.
7. Do not add conversational prose."#;

const MARKDOWN_GENERATOR_PREAMBLE: &str = r#"You are a technical knowledge-base author.
Your task is to convert a structured knowledge inventory into a comprehensive Markdown document.
Rules:
1. Output ONLY valid Markdown starting with a level-1 heading.
2. Include sections: Summary, Architecture, Database Schema, API Endpoints, Business Rules, Cross-References.
3. Use tables for database schema and API endpoints.
4. Use mermaid code blocks for architecture diagrams when applicable.
5. Be thorough — capture all details from the inventory.
6. Do not add conversational prose outside the Markdown."#;

const TECH_STACK_BLOCK: &str = r#"
## Target Technology Stack
- **Backend**: .NET 10
- **Frontend**: React + Vite + Tailwind CSS v4 + shadcn/ui
- **Database**: PostgreSQL 16
- **Cache**: Redis 7.x / 8.x
- **Auth**: Keycloak 26.x
- **Orchestration**: Docker Compose v2

When generating documentation, reference these technologies by name where relevant.
"#;

fn create_client(url: &str) -> Result<ollama::Client> {
    ollama::Client::builder()
        .api_key(Nothing)
        .base_url(url)
        .build()
        .with_context(|| format!("Failed to create client for {}", url))
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Markdown Knowledge-Base Pipeline Test ===\n");

    let extractor_client = create_client("http://localhost:11435")?;
    let generator_client = create_client("http://localhost:11434")?;

    let raw_text = "\
        This document describes the User Registration subsystem for AdComms. \
        The system uses PostgreSQL 16 as the primary data store. \
        The users table has columns: id (UUID), email (VARCHAR 255), password_hash (TEXT), \
        created_at (TIMESTAMPTZ), status (ENUM: active, suspended, deleted). \
        Registration flow: \
        1. User fills in the registration form (name, email, password). \
        2. Frontend sends POST /api/v1/auth/register to the .NET 10 backend. \
        3. Backend validates inputs and checks for duplicate emails. \
        4. If valid, a new row is inserted into the users table. \
        5. A verification email is sent via the Notification Service. \
        6. User clicks the verification link; backend marks status = active. \
        Business rules: passwords must be at least 12 characters with one uppercase, \
        one digit, and one special character. Accounts inactive for 90 days are suspended. \
        Architecture: React+Vite frontend -> .NET 10 API -> PostgreSQL, \
        with Redis 7 for session caching and Keycloak 26 for SSO.";

    // Step 1: Extraction
    println!("--- Step 1: Extraction ---");
    let extractor_preamble = format!("{}{}", TECH_STACK_BLOCK, MARKDOWN_EXTRACTOR_PREAMBLE);
    let extract_agent = extractor_client
        .agent("qwen2.5-coder:7b")
        .preamble(&extractor_preamble)
        .build();

    let extract_prompt = format!(
        "=== Document: user_registration.docx (word) ===\n{raw_text}\n\n\
         Extract the structured knowledge inventory from this document."
    );

    match tokio::time::timeout(
        std::time::Duration::from_secs(180),
        extract_agent.prompt(extract_prompt.as_str()),
    )
    .await
    {
        Ok(Ok(summary)) => {
            println!(
                "Extraction OK ({} chars)\n{}\n",
                summary.len(),
                &summary[..summary.len().min(300)]
            );

            // Step 2: Generation
            println!("--- Step 2: Markdown Generation ---");
            let generator_preamble =
                format!("{}{}", TECH_STACK_BLOCK, MARKDOWN_GENERATOR_PREAMBLE);
            let gen_agent = generator_client
                .agent("qwen2.5-coder:7b")
                .preamble(&generator_preamble)
                .build();

            let gen_prompt = format!(
                "=== Structured Summary ===\n{summary}\n\n\
                 Generate the Markdown knowledge-base document for: user_registration.docx"
            );

            match tokio::time::timeout(
                std::time::Duration::from_secs(240),
                gen_agent.prompt(gen_prompt.as_str()),
            )
            .await
            {
                Ok(Ok(markdown)) => {
                    println!("Generation OK:\n");
                    println!("{}", markdown);
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
