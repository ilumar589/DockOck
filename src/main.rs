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
mod chat;
mod context;
mod depgraph;
mod gherkin;
mod llm;
mod markdown;
mod mcp;
mod memory;
mod openspec;
mod parser;
mod rag;
mod session;
mod tech_stack;
mod validation;

/// Initialise structured tracing with `EnvFilter` and optional OTEL export.
///
/// Verbosity is controlled by the `RUST_LOG` environment variable.
/// Default: `info,dockock=debug,rig=info`.
fn init_tracing() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,dockock=debug,rig=info".into());

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    #[cfg(feature = "otel")]
    let registry = {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()
            .expect("OTLP exporter");
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name("dockock")
                    .build(),
            )
            .build();
        let tracer = provider.tracer("dockock");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        registry.with(otel_layer)
    };

    registry.init();
}

fn main() -> eframe::Result<()> {
    // Load .env file (API keys, custom provider settings)
    dotenv::dotenv().ok();

    // Initialise structured tracing with configurable verbosity
    init_tracing();

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
