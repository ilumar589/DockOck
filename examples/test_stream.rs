use anyhow::Result;
use futures::StreamExt;
use rig::agent::{MultiTurnStreamItem, Text};
use rig::client::{CompletionClient, Nothing};
use rig::providers::ollama;
use rig::streaming::{StreamedAssistantContent, StreamingPrompt};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Streaming pipeline test ===\n");

    let client = ollama::Client::builder()
        .api_key(Nothing)
        .base_url("http://localhost:11434")
        .build()?;

    let agent = client
        .agent("llama3.2")
        .preamble("You are a helpful assistant. Respond concisely.")
        .build();

    println!("--- Streaming response ---");
    let mut stream = agent.stream_prompt("What is 2+2? Give a one sentence answer.").await;

    let mut full_text = String::new();
    let mut chunk_count = 0usize;

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(Text { text }),
            )) => {
                print!("{text}");
                full_text.push_str(&text);
                chunk_count += 1;
            }
            Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                println!("\n\n--- Final response text: \"{}\" ---", res.response());
                break;
            }
            Err(e) => {
                println!("\nStream error: {:?}", e);
                break;
            }
            _ => {}
        }
    }

    println!("Accumulated: \"{}\" ({} chunks)", full_text, chunk_count);
    println!("\n=== Done ===");
    Ok(())
}
