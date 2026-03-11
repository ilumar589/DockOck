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
use tracing::{info, warn};

use crate::context::ProjectContext;
use crate::gherkin::GherkinDocument;

/// A timestamped log entry.
#[derive(Debug, Clone)]
struct LogEntry {
    timestamp: String,
    message: String,
    level: LogLevel,
}

#[derive(Debug, Clone, PartialEq)]
enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl LogLevel {
    fn color(&self) -> egui::Color32 {
        match self {
            Self::Info => egui::Color32::from_rgb(180, 180, 180),
            Self::Success => egui::Color32::from_rgb(100, 200, 100),
            Self::Warning => egui::Color32::from_rgb(230, 180, 60),
            Self::Error => egui::Color32::from_rgb(230, 80, 80),
        }
    }
}

fn now_timestamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() % 86400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

// ─────────────────────────────────────────────
// Events sent from background thread → UI
// ─────────────────────────────────────────────

/// Messages sent from the background processing task back to the UI thread.
#[derive(Debug)]
pub enum ProcessingEvent {
    /// Progress update message
    Status(String),
    /// A file has started LLM processing (used to animate the progress bar)
    FileStarted(PathBuf),
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
    log_entries: Vec<LogEntry>,
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
    /// User-selected output directory for saving .feature files
    output_dir: Option<PathBuf>,
    /// Processing progress: (files_completed, total_files)
    progress: (usize, usize),
    /// Number of files that have started LLM processing (for sub-unit progress)
    files_started: usize,
    /// Whether the log panel is expanded
    show_log_panel: bool,
    /// Toast-style notification message and remaining display time
    toast: Option<(String, f32)>,
    /// Pipeline mode: Fast (1 LLM call), Standard (2), Full (3)
    pipeline_mode: crate::llm::PipelineMode,
}

impl DockOckApp {
    /// Construct the app.  `runtime` must be a handle to an existing tokio Runtime.
    pub fn new(runtime: tokio::runtime::Handle, _cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            selected_files: Vec::new(),
            selected_index: None,
            results: HashMap::new(),
            log_entries: Vec::new(),
            state: AppState::Idle,
            ollama_ok: None,
            model_name: crate::llm::DEFAULT_MODEL.to_string(),
            event_rx: None,
            context: Arc::new(Mutex::new(ProjectContext::new())),
            runtime,
            output_dir: None,
            progress: (0, 0),
            files_started: 0,
            show_log_panel: true,
            toast: None,
            pipeline_mode: crate::llm::PipelineMode::default(),
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn log(&mut self, level: LogLevel, msg: impl Into<String>) {
        let message = msg.into();
        match level {
            LogLevel::Info => info!("{}", message),
            LogLevel::Success => info!("{}", message),
            LogLevel::Warning => warn!("{}", message),
            LogLevel::Error => tracing::error!("{}", message),
        }
        self.log_entries.push(LogEntry {
            timestamp: now_timestamp(),
            message,
            level,
        });
    }

    fn push_status(&mut self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg);
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
        self.log_entries.clear();
        self.selected_index = None;
        self.state = AppState::Idle;
        self.progress = (0, 0);
        self.files_started = 0;
        if let Ok(mut ctx) = self.context.lock() {
            ctx.clear();
        }
    }

    /// Save a single Gherkin document to the output directory.
    fn save_feature_file(&mut self, path: &PathBuf, doc: &GherkinDocument) {
        let dir = match &self.output_dir {
            Some(d) => d.clone(),
            None => {
                self.log(LogLevel::Warning, "No output directory selected. Please choose one first.");
                return;
            }
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        let out_path = dir.join(format!("{}.feature", stem));
        match std::fs::write(&out_path, doc.to_feature_string()) {
            Ok(()) => {
                self.log(LogLevel::Success, format!("Saved: {}", out_path.display()));
                self.toast = Some((format!("Saved {}.feature", stem), 3.0));
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("Failed to save {}: {}", out_path.display(), e));
            }
        }
    }

    /// Save all generated feature files to the output directory.
    fn save_all_feature_files(&mut self) {
        if self.output_dir.is_none() {
            self.log(LogLevel::Warning, "No output directory selected. Please choose one first.");
            return;
        }
        let pairs: Vec<_> = self.results.iter().map(|(p, d)| (p.clone(), d.clone())).collect();
        if pairs.is_empty() {
            self.log(LogLevel::Warning, "No generated Gherkin to save.");
            return;
        }
        let count = pairs.len();
        for (path, doc) in &pairs {
            self.save_feature_file(path, doc);
        }
        self.log(LogLevel::Success, format!("Saved {} .feature file(s)", count));
    }

    /// Kick off background processing for all selected files.
    fn start_processing(&mut self) {
        if self.selected_files.is_empty() {
            self.push_status("⚠ No files selected.");
            return;
        }

        self.state = AppState::Processing;
        self.results.clear();
        self.log_entries.clear();
        self.progress = (0, self.selected_files.len());
        self.files_started = 0;
        if let Ok(mut ctx) = self.context.lock() {
            ctx.clear();
        }

        let (tx, rx): (Sender<ProcessingEvent>, Receiver<ProcessingEvent>) = mpsc::channel();
        self.event_rx = Some(rx);

        let files = self.selected_files.clone();
        let context = Arc::clone(&self.context);
        let model = self.model_name.clone();
        let handle = self.runtime.clone();
        let mode = self.pipeline_mode;

        // Spawn a blocking thread that drives the async work
        std::thread::spawn(move || {
            handle.block_on(process_files(files, context, model, mode, tx));
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
                ProcessingEvent::FileStarted(_path) => {
                    self.files_started += 1;
                }
                ProcessingEvent::FileResult { path, gherkin } => {
                    self.progress.0 += 1;
                    self.log(LogLevel::Success, format!(
                        "✓ Generated Gherkin for: {} ({}/{})",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        self.progress.0,
                        self.progress.1,
                    ));
                    self.results.insert(path, gherkin);
                }
                ProcessingEvent::Done(result) => {
                    self.event_rx = None;
                    self.state = AppState::Done;
                    match result {
                        Ok(()) => {
                            self.log(LogLevel::Success, format!(
                                "✅ All {} files processed successfully.",
                                self.progress.1
                            ));
                            self.toast = Some(("Processing complete!".to_string(), 4.0));
                        }
                        Err(e) => self.log(LogLevel::Error, format!("❌ Processing failed: {}", e)),
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
            ui.separator();
            ui.label("Pipeline:");
            egui::ComboBox::from_id_salt("pipeline_mode")
                .selected_text(self.pipeline_mode.to_string())
                .show_ui(ui, |ui| {
                    for mode in crate::llm::PipelineMode::ALL {
                        ui.selectable_value(&mut self.pipeline_mode, mode, mode.to_string());
                    }
                });
            ui.separator();
            ui.label("📁 Output:");
            if let Some(dir) = &self.output_dir {
                let dir_display = dir.to_string_lossy();
                ui.label(dir_display.as_ref()).on_hover_text(dir_display.as_ref());
                if ui.small_button("✖").on_hover_text("Clear output directory").clicked() {
                    self.output_dir = None;
                }
            } else {
                ui.colored_label(egui::Color32::from_rgb(180, 180, 60), "Not set");
            }
            if ui.button("Browse…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.log(LogLevel::Info, format!("Output directory: {}", dir.display()));
                    self.output_dir = Some(dir);
                }
            }
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

        // Progress bar during processing
        if self.state == AppState::Processing && self.progress.1 > 0 {
            ui.add_space(4.0);
            // Each file contributes 1 unit when complete and 0.5 when started.
            // This ensures the bar is non-zero as soon as LLM processing begins.
            let completed = self.progress.0 as f32;
            let started = self.files_started.saturating_sub(self.progress.0) as f32;
            let total = self.progress.1 as f32;
            let fraction = ((completed + started * 0.5) / total).clamp(0.0, 1.0);
            let bar = egui::ProgressBar::new(fraction)
                .text(format!("{}/{} files", self.progress.0, self.progress.1))
                .animate(true);
            ui.add(bar);
        }

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
                                ui.close();
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
                        ui.ctx().copy_text(text.clone());
                        self.toast = Some(("Copied to clipboard".to_string(), 2.0));
                    }
                    let can_save = self.output_dir.is_some();
                    if ui
                        .add_enabled(can_save, egui::Button::new("💾 Save"))
                        .on_hover_text(if can_save { "Save this .feature file" } else { "Set output directory first" })
                        .clicked()
                    {
                        if let Some(idx) = self.selected_index {
                            if let Some(path) = self.selected_files.get(idx).cloned() {
                                if let Some(doc) = self.results.get(&path).cloned() {
                                    self.save_feature_file(&path, &doc);
                                }
                            }
                        }
                    }
                    if ui
                        .add_enabled(can_save && !self.results.is_empty(), egui::Button::new("💾 Save All"))
                        .on_hover_text(if can_save { "Save all .feature files" } else { "Set output directory first" })
                        .clicked()
                    {
                        self.save_all_feature_files();
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
                let pct = if self.progress.1 > 0 {
                    let completed = self.progress.0 as f32;
                    let started = self.files_started.saturating_sub(self.progress.0) as f32;
                    let total = self.progress.1 as f32;
                    ((completed + started * 0.5) / total * 100.0).clamp(0.0, 100.0) as u32
                } else {
                    0
                };
                ui.label(format!("Processing… {}%", pct));
            }

            ui.separator();

            // Toggle log panel
            let log_label = if self.show_log_panel { "▼ Log" } else { "▶ Log" };
            if ui.button(log_label).clicked() {
                self.show_log_panel = !self.show_log_panel;
            }

            if !self.log_entries.is_empty() {
                ui.label(format!("({} entries)", self.log_entries.len()));
            }

            ui.separator();

            // Show last status message
            if let Some(entry) = self.log_entries.last() {
                ui.colored_label(entry.level.color(), &entry.message);
            }
        });
    }

    fn render_log_panel(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("log_panel")
            .stick_to_bottom(true)
            .max_height(150.0)
            .show(ui, |ui| {
                for entry in &self.log_entries {
                    ui.horizontal(|ui| {
                        ui.monospace(
                            egui::RichText::new(&entry.timestamp)
                                .color(egui::Color32::from_rgb(120, 120, 120)),
                        );
                        ui.colored_label(entry.level.color(), &entry.message);
                    });
                }
            });
    }

    fn render_toast(&mut self, ctx: &egui::Context) {
        if let Some((msg, remaining)) = &mut self.toast {
            egui::Area::new(egui::Id::new("toast_notification"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, 40.0])
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(40, 40, 40, 230))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(msg.as_str())
                                    .color(egui::Color32::WHITE)
                                    .size(14.0),
                            );
                        });
                });
            *remaining -= ctx.input(|i| i.unstable_dt);
            if *remaining <= 0.0 {
                self.toast = None;
            }
            ctx.request_repaint();
        }
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

        if self.show_log_panel {
            egui::TopBottomPanel::bottom("log_panel")
                .resizable(true)
                .default_height(120.0)
                .show(ctx, |ui| {
                    self.render_log_panel(ui);
                });
        }

        egui::SidePanel::left("left_panel")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                self.render_left_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_right_panel(ui);
        });

        // Toast overlay
        self.render_toast(ctx);
    }
}

// ─────────────────────────────────────────────
// Background processing task
// ─────────────────────────────────────────────

/// Async task that parses all files in parallel, then runs them through the
/// multi-agent pipeline (Extract → Generate → Review) concurrently.
async fn process_files(
    files: Vec<PathBuf>,
    context: Arc<Mutex<ProjectContext>>,
    model: String,
    mode: crate::llm::PipelineMode,
    tx: Sender<ProcessingEvent>,
) {
    let total = files.len();

    // ── Phase 0: Spin up the orchestrator and probe all Ollama instances ──
    let _ = tx.send(ProcessingEvent::Status(
        "🔌 Probing Ollama instances…".to_string(),
    ));

    let (orchestrator, statuses) = match crate::llm::AgentOrchestrator::new(&model, mode).await {
        Ok(pair) => pair,
        Err(e) => {
            let _ = tx.send(ProcessingEvent::Done(Err(e.to_string())));
            return;
        }
    };

    for st in &statuses {
        let symbol = if st.reachable { "●" } else { "○" };
        let _ = tx.send(ProcessingEvent::Status(format!(
            "{} {} ({}): {}",
            symbol,
            st.name,
            st.url,
            if st.reachable { "online" } else { "offline — will fallback" },
        )));
    }

    let orchestrator = Arc::new(orchestrator);

    // ── Phase 1: Parse ALL files in parallel (CPU/IO bound, no LLM) ──
    let _ = tx.send(ProcessingEvent::Status(format!(
        "📄 Parsing {} files in parallel…", total
    )));

    let mut parse_handles = Vec::with_capacity(total);
    for path in &files {
        let p = path.clone();
        parse_handles.push(tokio::task::spawn_blocking(move || {
            crate::parser::parse_file(&p).map(|r| (p, r.0, r.1))
        }));
    }

    // Collect parsed results
    let mut parsed_files: Vec<(PathBuf, String, String)> = Vec::with_capacity(total);
    for handle in parse_handles {
        match handle.await {
            Ok(Ok((path, file_type, raw_text))) => {
                let name = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let _ = tx.send(ProcessingEvent::Status(format!("📄 Parsed: {}", name)));

                // Store in shared context
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

                parsed_files.push((path, file_type, raw_text));
            }
            Ok(Err(e)) => {
                let _ = tx.send(ProcessingEvent::Status(format!("⚠ Parse error: {}", e)));
            }
            Err(e) => {
                let _ = tx.send(ProcessingEvent::Status(format!("⚠ Parse task panicked: {}", e)));
            }
        }
    }

    let _ = tx.send(ProcessingEvent::Status(format!(
        "✅ Parsed {}/{} files. Starting multi-agent pipeline…",
        parsed_files.len(),
        total
    )));

    // ── Phase 2: Run agent pipeline concurrently ──
    // Take a snapshot of context now (after all files are parsed)
    let ctx_snapshot = context.lock().map(|c| c.clone()).unwrap_or_default();

    let mut llm_handles = Vec::with_capacity(parsed_files.len());
    for (path, file_type, raw_text) in parsed_files {
        let orch = Arc::clone(&orchestrator);
        let sem = Arc::clone(&orchestrator.semaphore);
        let tx = tx.clone();
        let ctx = ctx_snapshot.clone();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let handle = tokio::spawn(async move {
            // Acquire semaphore permit to limit concurrency
            let _permit = sem.acquire().await;

            // Signal the UI that this file has started LLM processing
            let _ = tx.send(ProcessingEvent::FileStarted(path.clone()));

            // Create a status channel that wraps strings into ProcessingEvent::Status
            let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
            let tx_fwd = tx.clone();
            // Spawn a small forwarder thread
            let fwd = std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    let _ = tx_fwd.send(ProcessingEvent::Status(msg));
                }
            });

            let result = orch
                .process_file(&file_name, &file_type, &raw_text, &ctx, &status_tx)
                .await;

            drop(status_tx); // signal forwarder to stop
            let _ = fwd.join();

            match result {
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
                        "⚠ Pipeline error for {}: {}",
                        file_name, e
                    )));
                }
            }
        });

        llm_handles.push(handle);
    }

    // Wait for all LLM tasks to complete
    for handle in llm_handles {
        let _ = handle.await;
    }

    let _ = tx.send(ProcessingEvent::Done(Ok(())));
}
