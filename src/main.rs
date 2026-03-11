//! DockOck – a desktop tool that parses Word, Visio and Excel files and
//! transforms them into per-file Gherkin documentation using a local Ollama LLM.
//!
//! ## Quick start
//!
//! 1. Start Ollama: `docker-compose up -d`  (or `ollama serve` directly)
//! 2. Run the app:  `cargo run --release`
//! 3. In the UI:
//!    - Click **➕ Add Files** to select `.docx`, `.xlsx`, or `.vsdx` files
//!    - Optionally change the Ollama model name
//!    - Click **⚙ Generate Gherkin**
//!    - Select any file in the left panel to view or copy its `.feature` output

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cache;
mod context;
mod gherkin;
mod llm;
mod openspec;
mod parser;

fn main() -> eframe::Result<()> {
    // Initialise tracing for debug logs
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // Create a Tokio runtime that lives for the duration of the process.
    // The UI runs on the main thread; async work is spawned via the runtime handle.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    let handle = runtime.handle().clone();

    // Keep the runtime alive by moving it into a background thread.
    std::thread::spawn(move || {
        let _runtime = runtime;
        // Block forever so the runtime (and its threads) stay alive.
        std::thread::park();
    });

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DockOck – Document → Gherkin")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([700.0, 450.0]),
        ..Default::default()
    };

    eframe::run_native(
        "DockOck",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::DockOckApp::new(handle, cc)))),
    )
}
