use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("debug,rig=trace,hyper=warn,reqwest=debug")
        .init();

    println!("=== Creating AgentOrchestrator ===");
    let (orch, statuses) = dockock::llm::AgentOrchestrator::new(
        "qwen2.5-coder:7b",
        "qwen2.5-coder:7b",
        "qwen2.5-coder:7b",
        "minicpm-v",
        dockock::llm::PipelineMode::default(),
    ).await?;

    for st in &statuses {
        println!("  {} ({}): {}", st.name, st.url, if st.reachable { "online" } else { "offline" });
    }

    let orch = Arc::new(orch);

    // Create a fake context
    let ctx = dockock::context::ProjectContext::default();

    // Use a small test document
    let raw_text = "This document describes a user login process. \
        The user enters their username and password on the login page. \
        The system validates the credentials against the database. \
        If valid, the user is redirected to the dashboard. \
        If invalid, an error message is shown.";

    let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();

    // Print status messages from a separate thread
    let printer = std::thread::spawn(move || {
        while let Ok(msg) = status_rx.recv() {
            println!("  STATUS: {}", msg);
        }
    });

    println!("=== Running pipeline ===");
    match orch.process_file("test_doc.docx", "word", raw_text, &[], &ctx, &status_tx).await {
        Ok(gherkin) => {
            println!("=== SUCCESS ===");
            println!("{}", gherkin);
        }
        Err(e) => {
            println!("=== FAILED ===");
            println!("Error: {:?}", e);
        }
    }

    drop(status_tx);
    let _ = printer.join();

    println!("=== Done ===");
    Ok(())
}
