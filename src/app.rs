//! egui application state and UI rendering.
//!
//! The application window is split into three areas:
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │  Top bar  (title + Ollama status)        │
//! ├──────────────────┬──────────────────────┤
//! │  Left panel      │  Right panel          │
//! │  File list       │  Gherkin output       │
//! │  [Add Files]     │  (selected file)      │
//! │  [Clear]         │                       │
//! ├──────────────────┴──────────────────────┤
//! │  Bottom bar (status + [Generate] btn)    │
//! └─────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::context::ProjectContext;
use crate::gherkin::GherkinDocument;

// ─────────────────────────────────────────────
// Events sent from background thread → UI
// ─────────────────────────────────────────────

/// Messages sent from the background processing task back to the UI thread.
#[derive(Debug)]
pub enum ProcessingEvent {
    /// Progress update message
    Status(String),
    /// A single file has been fully processed
    FileResult {
        path: PathBuf,
        gherkin: GherkinDocument,
    },
    /// All files have been processed (or an error terminated the run)
    Done(Result<(), String>),
}

// ─────────────────────────────────────────────
// App state
// ─────────────────────────────────────────────

/// Tracks which step we are in.
#[derive(Debug, Default, PartialEq)]
enum AppState {
    #[default]
    Idle,
    Processing,
    Done,
}

/// Main application state owned by the egui event loop.
pub struct DockOckApp {
    /// Files selected by the user
    selected_files: Vec<PathBuf>,
    /// Index into `selected_files` for the currently displayed result
    selected_index: Option<usize>,
    /// Generated Gherkin documents keyed by file path
    results: HashMap<PathBuf, GherkinDocument>,
    /// Current status / log messages
    status_messages: Vec<String>,
    /// Current processing state
    state: AppState,
    /// Ollama status: None = not checked, Some(true) = reachable, Some(false) = unreachable
    ollama_ok: Option<bool>,
    /// Ollama model name to use
    model_name: String,
    /// Channel receiver for background processing events
    event_rx: Option<Receiver<ProcessingEvent>>,
    /// Shared context accumulator (wrapped in Arc<Mutex<>> so background thread can write)
    context: Arc<Mutex<ProjectContext>>,
    /// Tokio runtime handle for spawning async tasks
    runtime: tokio::runtime::Handle,
}

impl DockOckApp {
    /// Construct the app.  `runtime` must be a handle to an existing tokio Runtime.
    pub fn new(runtime: tokio::runtime::Handle, _cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            selected_files: Vec::new(),
            selected_index: None,
            results: HashMap::new(),
            status_messages: Vec::new(),
            state: AppState::Idle,
            ollama_ok: None,
            model_name: crate::llm::DEFAULT_MODEL.to_string(),
            event_rx: None,
            context: Arc::new(Mutex::new(ProjectContext::new())),
            runtime,
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn push_status(&mut self, msg: impl Into<String>) {
        self.status_messages.push(msg.into());
    }

    /// Open a multi-file dialog and append chosen files to the list.
    fn open_file_dialog(&mut self) {
        let paths = rfd::FileDialog::new()
            .add_filter(
                "Supported documents",
                &["docx", "xlsx", "xls", "xlsm", "xlsb", "ods", "vsdx", "vsd", "vsdm"],
            )
            .pick_files();

        if let Some(paths) = paths {
            for p in paths {
                if !self.selected_files.contains(&p) {
                    self.push_status(format!("Added: {}", p.display()));
                    self.selected_files.push(p);
                }
            }
        }
    }

    /// Clear the file list and all results.
    fn clear_all(&mut self) {
        self.selected_files.clear();
        self.results.clear();
        self.status_messages.clear();
        self.selected_index = None;
        self.state = AppState::Idle;
        if let Ok(mut ctx) = self.context.lock() {
            ctx.clear();
        }
    }

    /// Kick off background processing for all selected files.
    fn start_processing(&mut self) {
        if self.selected_files.is_empty() {
            self.push_status("⚠ No files selected.");
            return;
        }

        self.state = AppState::Processing;
        self.results.clear();
        self.status_messages.clear();
        if let Ok(mut ctx) = self.context.lock() {
            ctx.clear();
        }

        let (tx, rx): (Sender<ProcessingEvent>, Receiver<ProcessingEvent>) = mpsc::channel();
        self.event_rx = Some(rx);

        let files = self.selected_files.clone();
        let context = Arc::clone(&self.context);
        let model = self.model_name.clone();
        let handle = self.runtime.clone();

        // Spawn a blocking thread that drives the async work
        std::thread::spawn(move || {
            handle.block_on(process_files(files, context, model, tx));
        });
    }

    /// Poll the event channel and apply any received events.
    fn poll_events(&mut self) {
        let events: Vec<ProcessingEvent> = if let Some(rx) = &self.event_rx {
            std::iter::from_fn(|| rx.try_recv().ok()).collect()
        } else {
            return;
        };

        for event in events {
            match event {
                ProcessingEvent::Status(msg) => {
                    self.push_status(msg);
                }
                ProcessingEvent::FileResult { path, gherkin } => {
                    self.push_status(format!(
                        "✓ Generated Gherkin for: {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    ));
                    self.results.insert(path, gherkin);
                }
                ProcessingEvent::Done(result) => {
                    self.event_rx = None;
                    self.state = AppState::Done;
                    match result {
                        Ok(()) => self.push_status("✅ All files processed successfully."),
                        Err(e) => self.push_status(format!("❌ Processing failed: {}", e)),
                    }
                }
            }
        }
    }

    /// Check Ollama availability in the background and update `self.ollama_ok`.
    fn check_ollama(&mut self, ctx: &egui::Context) {
        let repaint = ctx.clone();
        let handle = self.runtime.clone();
        let status_tx = {
            // Use a simple one-shot channel
            let (tx, rx) = mpsc::channel::<bool>();
            std::thread::spawn(move || {
                let ok = handle
                    .block_on(crate::llm::check_ollama_connection())
                    .is_ok();
                let _ = tx.send(ok);
                repaint.request_repaint();
            });
            rx
        };

        // Store the receiver so we can poll on the next frame
        // For simplicity we do a blocking poll right away (the check is fast)
        if let Ok(ok) = status_tx.recv_timeout(std::time::Duration::from_secs(4)) {
            self.ollama_ok = Some(ok);
        }
    }

    // ── UI rendering ─────────────────────────────────────────────────────

    fn render_top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("🦆 DockOck");
            ui.separator();
            ui.label("Ollama:");
            match self.ollama_ok {
                None => {
                    if ui.button("Check connection").clicked() {
                        self.check_ollama(ctx);
                    }
                }
                Some(true) => {
                    ui.colored_label(egui::Color32::GREEN, "● Connected");
                }
                Some(false) => {
                    ui.colored_label(egui::Color32::RED, "● Unreachable");
                    if ui.button("Retry").clicked() {
                        self.ollama_ok = None;
                        self.check_ollama(ctx);
                    }
                }
            }
            ui.separator();
            ui.label("Model:");
            ui.text_edit_singleline(&mut self.model_name);
        });
    }

    fn render_left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("📂 Files");
        ui.separator();

        let is_processing = self.state == AppState::Processing;

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!is_processing, egui::Button::new("➕ Add Files"))
                .clicked()
            {
                self.open_file_dialog();
            }
            if ui
                .add_enabled(!is_processing, egui::Button::new("🗑 Clear"))
                .clicked()
            {
                self.clear_all();
            }
        });

        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("file_list")
            .max_height(ui.available_height() - 60.0)
            .show(ui, |ui| {
                let mut to_remove: Option<usize> = None;

                for (i, path) in self.selected_files.iter().enumerate() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());

                    let has_result = self.results.contains_key(path);
                    let label = if has_result {
                        format!("✓ {}", name)
                    } else {
                        name.clone()
                    };

                    let selected = self.selected_index == Some(i);
                    let resp = ui
                        .selectable_label(selected, &label)
                        .on_hover_text(path.to_string_lossy().as_ref());

                    if resp.clicked() {
                        self.selected_index = Some(i);
                    }

                    if !is_processing {
                        resp.context_menu(|ui| {
                            if ui.button("Remove").clicked() {
                                to_remove = Some(i);
                                ui.close_menu();
                            }
                        });
                    }
                }

                if let Some(idx) = to_remove {
                    let path = self.selected_files.remove(idx);
                    self.results.remove(&path);
                    if self.selected_index == Some(idx) {
                        self.selected_index = None;
                    }
                }
            });
    }

    fn render_right_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("📝 Gherkin Output");
        ui.separator();

        let content = if let Some(idx) = self.selected_index {
            self.selected_files
                .get(idx)
                .and_then(|p| self.results.get(p))
                .map(|doc| doc.to_feature_string())
        } else {
            None
        };

        match content {
            Some(text) => {
                ui.horizontal(|ui| {
                    if ui.button("📋 Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = text.clone());
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("gherkin_scroll")
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text.as_str())
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                    });
            }
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    if self.selected_files.is_empty() {
                        ui.label("Add files using the ➕ button and click Generate.");
                    } else if self.state == AppState::Processing {
                        ui.label("⏳ Processing files…");
                        ui.spinner();
                    } else {
                        ui.label("Select a file on the left to see its Gherkin output.");
                    }
                });
            }
        }
    }

    fn render_bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.horizontal(|ui| {
            let is_processing = self.state == AppState::Processing;
            let has_files = !self.selected_files.is_empty();

            if ui
                .add_enabled(
                    !is_processing && has_files,
                    egui::Button::new("⚙ Generate Gherkin"),
                )
                .clicked()
            {
                self.start_processing();
            }

            if is_processing {
                ui.spinner();
                ui.label("Processing…");
            }

            ui.separator();

            // Show last few status messages
            if let Some(msg) = self.status_messages.last() {
                ui.label(msg);
            }
        });
    }
}

impl eframe::App for DockOckApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll background events every frame
        self.poll_events();
        if self.state == AppState::Processing {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            self.render_top_bar(ui, ctx);
        });

        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            self.render_bottom_bar(ui);
        });

        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                self.render_left_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_right_panel(ui);
        });
    }
}

// ─────────────────────────────────────────────
// Background processing task
// ─────────────────────────────────────────────

/// Async task that parses all files, accumulates context, and generates Gherkin.
async fn process_files(
    files: Vec<PathBuf>,
    context: Arc<Mutex<ProjectContext>>,
    model: String,
    tx: Sender<ProcessingEvent>,
) {
    let total = files.len();

    for (i, path) in files.iter().enumerate() {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        let _ = tx.send(ProcessingEvent::Status(format!(
            "Parsing {} ({}/{})",
            file_name,
            i + 1,
            total
        )));

        // --- Parse the file (blocking I/O – run on thread pool) ---
        let path_clone = path.clone();
        let parse_result =
            tokio::task::spawn_blocking(move || crate::parser::parse_file(&path_clone))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));

        let (file_type, raw_text) = match parse_result {
            Ok(pair) => pair,
            Err(e) => {
                let _ = tx.send(ProcessingEvent::Status(format!(
                    "⚠ Failed to parse {}: {}",
                    file_name, e
                )));
                continue;
            }
        };

        // Store this file's content in the shared context
        {
            let content = crate::context::FileContent {
                path: path.clone(),
                file_type: file_type.clone(),
                raw_text: raw_text.clone(),
            };
            if let Ok(mut ctx) = context.lock() {
                ctx.add_file(content);
            }
        }

        // --- Generate Gherkin via LLM ---
        let _ = tx.send(ProcessingEvent::Status(format!(
            "Generating Gherkin for {} via Ollama…",
            file_name
        )));

        let ctx_snapshot = context.lock().map(|c| c.clone()).unwrap_or_default();
        let llm_result = crate::llm::generate_gherkin(
            &file_name,
            &file_type,
            &raw_text,
            &ctx_snapshot,
            Some(&model),
        )
        .await;

        match llm_result {
            Ok(raw_gherkin) => {
                let doc = crate::gherkin::GherkinDocument::parse_from_llm_output(
                    &raw_gherkin,
                    &file_name,
                );
                let _ = tx.send(ProcessingEvent::FileResult {
                    path: path.clone(),
                    gherkin: doc,
                });
            }
            Err(e) => {
                let _ = tx.send(ProcessingEvent::Status(format!(
                    "⚠ LLM error for {}: {}",
                    file_name, e
                )));
            }
        }
    }

    let _ = tx.send(ProcessingEvent::Done(Ok(())));
}
