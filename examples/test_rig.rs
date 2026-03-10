use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama;

#[tokio::main]
async fn main() {
    // Test 1: default client (port 11434)
    println!("=== Test 1: Default client (builder, no base_url override) ===");
    let default_client = ollama::Client::builder()
        .api_key(Nothing)
        .build()
        .expect("default client");
    let agent = default_client
        .agent("llama3.2")
        .preamble("You are a helpful assistant.")
        .build();
    match agent.prompt("Say hello in one word.").await {
        Ok(resp) => println!("  OK: {}", resp),
        Err(e) => println!("  ERR: {:?}", e),
    }

    // Test 2: builder with explicit base_url for port 11434
    println!("=== Test 2: Builder base_url http://localhost:11434 ===");
    let client_434 = ollama::Client::builder()
        .api_key(Nothing)
        .base_url("http://localhost:11434")
        .build()
        .expect("client 11434");
    let agent = client_434
        .agent("llama3.2")
        .preamble("You are a helpful assistant.")
        .build();
    match agent.prompt("Say hello in one word.").await {
        Ok(resp) => println!("  OK: {}", resp),
        Err(e) => println!("  ERR: {:?}", e),
    }

    // Test 3: builder with base_url for port 11435
    println!("=== Test 3: Builder base_url http://localhost:11435 ===");
    let client_435 = ollama::Client::builder()
        .api_key(Nothing)
        .base_url("http://localhost:11435")
        .build()
        .expect("client 11435");
    let agent = client_435
        .agent("llama3.2")
        .preamble("You are a helpful assistant.")
        .build();
    match agent.prompt("Say hello in one word.").await {
        Ok(resp) => println!("  OK: {}", resp),
        Err(e) => println!("  ERR: {:?}", e),
    }

    println!("=== Done ===");
}
