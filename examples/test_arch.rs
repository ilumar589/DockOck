use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama;
use std::sync::Arc;

struct SimpleOrchestrator {
    client: ollama::Client,
}

impl SimpleOrchestrator {
    fn new() -> Self {
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url("http://localhost:11434")
            .build()
            .expect("build client");
        Self { client }
    }

    async fn call(&self, text: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let agent = self
            .client
            .agent("llama3.2")
            .preamble("You are a helpful assistant. Respond in one sentence.")
            .build();

        let resp = agent.prompt(text).await?;
        Ok(resp)
    }
}

#[tokio::main]
async fn main() {
    println!("=== Test: Simulating app architecture ===");

    // Create runtime handle like the app does
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let handle = rt.handle().clone();

    // Keep runtime alive
    std::thread::spawn(move || {
        let _rt = rt;
        std::thread::park();
    });

    // Simulate start_processing: spawn a new thread that block_on's the async work
    let (result_tx, result_rx) = std::sync::mpsc::channel::<String>();

    std::thread::spawn(move || {
        handle.block_on(async {
            let orch = Arc::new(SimpleOrchestrator::new());

            // Spawn multiple tokio tasks like process_files does
            let mut handles = vec![];
            for i in 0..3 {
                let o = Arc::clone(&orch);
                let tx = result_tx.clone();
                let h = tokio::spawn(async move {
                    let prompt = format!("What is {} + {}?", i, i + 1);
                    match o.call(&prompt).await {
                        Ok(resp) => {
                            let _ = tx.send(format!("Task {}: OK - {}", i, resp));
                        }
                        Err(e) => {
                            let _ = tx.send(format!("Task {}: ERR - {:?}", i, e));
                        }
                    }
                });
                handles.push(h);
            }
            drop(result_tx);

            for h in handles {
                let _ = h.await;
            }
        });
    });

    // Read results
    while let Ok(msg) = result_rx.recv() {
        println!("  {}", msg);
    }

    println!("=== Done ===");
}
