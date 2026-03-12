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

use crate::context::{FileGroup, ProjectContext};
use crate::gherkin::GherkinDocument;
use std::collections::HashSet;

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
// Known model presets
// ─────────────────────────────────────────────

const KNOWN_MODELS: &[&str] = &[
    "qwen2.5-coder:32b",
    "qwen2.5-coder:7b",
    "qwen2.5-coder:3b",
    "deepseek-coder-v2:16b",
    "codellama:34b",
    "codellama:13b",
    "codellama:7b",
    "mistral-small:24b",
    "phi3:14b",
    "phi3:mini",
    "gemma2:9b",
    "gemma2:2b",
    "llama3.2",
    "llama3.1:8b",
    "llama3.1:70b",
    // Vision models
    "minicpm-v",
    "llava:7b",
    "llava:13b",
    "moondream",
    "llama3.2-vision",
];

/// Editable combo box for model selection — pick from presets or type a custom name.
fn model_combo(ui: &mut egui::Ui, id: &str, model: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(model.as_str())
        .width(160.0)
        .show_ui(ui, |ui| {
            for &name in KNOWN_MODELS {
                ui.selectable_value(model, name.to_string(), name);
            }
        });
}

/// Combo box for custom provider models loaded from custom_providers.json.
fn custom_model_combo(ui: &mut egui::Ui, id: &str, model: &mut String, models: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(model.as_str())
        .width(160.0)
        .show_ui(ui, |ui| {
            for name in models {
                ui.selectable_value(model, name.clone(), name.as_str());
            }
        });
}

// ─────────────────────────────────────────────
// Events sent from background thread → UI
// ─────────────────────────────────────────────

/// Messages sent from the background processing task back to the UI thread.
#[derive(Debug)]
pub enum ProcessingEvent {
    /// Progress update message
    Status(String),
    /// A file/group has started LLM processing (used to animate the progress bar)
    FileStarted(PathBuf),
    /// A single file has been fully processed
    FileResult {
        path: PathBuf,
        gherkin: GherkinDocument,
        elapsed: std::time::Duration,
    },
    /// A group of files has been fully processed
    GroupResult {
        group_name: String,
        gherkin: GherkinDocument,
        elapsed: std::time::Duration,
    },
    /// All files have been processed (or an error terminated the run)
    Done(Result<(), String>),
    /// OpenSpec export started
    OpenSpecStarted,
    /// OpenSpec export completed for one document
    OpenSpecResult {
        change_name: String,
        result: crate::openspec::OpenSpecExportResult,
    },
    /// OpenSpec export phase finished
    OpenSpecDone(Result<usize, String>),
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

/// Identifies what is currently selected in the left panel.
#[derive(Debug, Clone, PartialEq)]
enum Selection {
    /// An individual (ungrouped) file by index in `selected_files`
    File(usize),
    /// A file group by name
    Group(String),
}

/// Main application state owned by the egui event loop.
pub struct DockOckApp {
    /// Files selected by the user
    selected_files: Vec<PathBuf>,
    /// What is currently selected in the left panel
    selection: Option<Selection>,
    /// Generated Gherkin documents keyed by file path
    results: HashMap<PathBuf, GherkinDocument>,
    /// Generated Gherkin documents for file groups, keyed by group name
    group_results: HashMap<String, GherkinDocument>,
    /// Elapsed generation time per file
    elapsed_times: HashMap<PathBuf, std::time::Duration>,
    /// Elapsed generation time per group
    group_elapsed_times: HashMap<String, std::time::Duration>,
    /// Current status / log messages
    log_entries: Vec<LogEntry>,
    /// Current processing state
    state: AppState,
    /// Ollama status: None = not checked, Some(true) = reachable, Some(false) = unreachable
    ollama_ok: Option<bool>,
    /// Ollama model name for the generator agent
    generator_model: String,
    /// Ollama model name for the extractor agent
    extractor_model: String,
    /// Ollama model name for the reviewer agent
    reviewer_model: String,
    /// Ollama model name for the vision agent (image description)
    vision_model: String,
    /// Channel receiver for background processing events
    event_rx: Option<Receiver<ProcessingEvent>>,
    /// Shared context accumulator (wrapped in Arc<Mutex<>> so background thread can write)
    context: Arc<Mutex<ProjectContext>>,
    /// Tokio runtime handle for spawning async tasks
    runtime: tokio::runtime::Handle,
    /// User-selected output directory for saving .feature files
    output_dir: Option<PathBuf>,
    /// Processing progress: (items_completed, total_items) — items = groups + ungrouped files
    progress: (usize, usize),
    /// Number of items that have started LLM processing (for sub-unit progress)
    files_started: usize,
    /// Whether the log panel is expanded
    show_log_panel: bool,
    /// Toast-style notification message and remaining display time
    toast: Option<(String, f32)>,
    /// Pipeline mode: Fast (1 LLM call), Standard (2), Full (3)
    pipeline_mode: crate::llm::PipelineMode,
    /// Whether auto-grouping by file stem is enabled
    auto_group_enabled: bool,
    /// File groups (auto-detected + manual)
    file_groups: Vec<FileGroup>,
    /// Name buffer for creating a new group
    new_group_name: String,
    /// Whether the new-group input is shown
    show_new_group_input: bool,
    /// Whether OpenSpec export is enabled (optional final phase)
    openspec_enabled: bool,
    /// Base URL for the OpenSpec service
    openspec_url: String,
    /// OpenSpec service status: None = not checked, Some(true) = reachable
    openspec_ok: Option<bool>,
    /// OpenSpec export results keyed by change name
    openspec_results: HashMap<String, crate::openspec::OpenSpecExportResult>,
    /// Maximum number of concurrent LLM tasks
    max_concurrent: usize,
    /// Force re-generation even if cache has valid entries
    force_regenerate: bool,
    /// User quality ratings keyed by file path string or group name
    ratings: HashMap<String, crate::session::Rating>,
    /// Previous results for diffing (before last regeneration)
    previous_results: HashMap<PathBuf, GherkinDocument>,
    /// Previous group results for diffing
    previous_group_results: HashMap<String, GherkinDocument>,
    /// Whether the diff view is active for the current selection
    show_diff: bool,
    /// Per-selection refinement instruction text
    refinement_input: String,
    /// Whether a session restore prompt should be shown
    session_restore_pending: bool,
    /// Active LLM backend (Ollama or Custom provider)
    backend: crate::llm::ProviderBackend,
    /// Loaded custom provider configurations from custom_providers.json
    custom_providers: Vec<crate::llm::CustomProviderConfig>,
    /// Saved Ollama model selections (restored when switching back from custom)
    saved_ollama_models: (String, String, String, String),
}

impl DockOckApp {
    /// Construct the app.  `runtime` must be a handle to an existing tokio Runtime.
    pub fn new(runtime: tokio::runtime::Handle, _cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            selected_files: Vec::new(),
            selection: None,
            results: HashMap::new(),
            group_results: HashMap::new(),
            elapsed_times: HashMap::new(),
            group_elapsed_times: HashMap::new(),
            log_entries: Vec::new(),
            state: AppState::Idle,
            ollama_ok: None,
            generator_model: crate::llm::DEFAULT_GENERATOR_MODEL.to_string(),
            extractor_model: crate::llm::DEFAULT_EXTRACTOR_MODEL.to_string(),
            reviewer_model: crate::llm::DEFAULT_REVIEWER_MODEL.to_string(),
            vision_model: crate::llm::DEFAULT_VISION_MODEL.to_string(),
            event_rx: None,
            context: Arc::new(Mutex::new(ProjectContext::new())),
            runtime,
            output_dir: None,
            progress: (0, 0),
            files_started: 0,
            show_log_panel: true,
            toast: None,
            pipeline_mode: crate::llm::PipelineMode::default(),
            auto_group_enabled: true,
            file_groups: Vec::new(),
            new_group_name: String::new(),
            show_new_group_input: false,
            openspec_enabled: false,
            openspec_url: crate::openspec::DEFAULT_OPENSPEC_URL.to_string(),
            openspec_ok: None,
            openspec_results: HashMap::new(),
            max_concurrent: crate::llm::DEFAULT_MAX_CONCURRENT,
            force_regenerate: false,
            ratings: HashMap::new(),
            previous_results: HashMap::new(),
            previous_group_results: HashMap::new(),
            show_diff: false,
            refinement_input: String::new(),
            session_restore_pending: false,
            backend: crate::llm::ProviderBackend::Ollama,
            custom_providers: crate::llm::load_custom_providers(
                &std::env::current_dir().unwrap_or_default(),
            ),
            saved_ollama_models: (
                crate::llm::DEFAULT_GENERATOR_MODEL.to_string(),
                crate::llm::DEFAULT_EXTRACTOR_MODEL.to_string(),
                crate::llm::DEFAULT_REVIEWER_MODEL.to_string(),
                crate::llm::DEFAULT_VISION_MODEL.to_string(),
            ),
        }
    }

    /// Recompute file groups.  Auto-groups are rebuilt from scratch; manual groups
    /// are preserved but stale members (files no longer in the selection) are pruned.
    fn recompute_groups(&mut self) {
        if self.auto_group_enabled {
            // Preserve manual groups (names that have no auto equivalent)
            let auto = crate::context::compute_auto_groups(&self.selected_files);
            let auto_names: HashSet<&str> = auto.iter().map(|g| g.name.as_str()).collect();

            // Keep manual groups that don't collide with auto names
            let manual: Vec<FileGroup> = self
                .file_groups
                .iter()
                .filter(|g| !auto_names.contains(g.name.as_str()))
                .cloned()
                .collect();

            self.file_groups = auto;
            self.file_groups.extend(manual);
        }

        // Prune members that are no longer in selected_files
        let selected_set: HashSet<PathBuf> = self.selected_files.iter().cloned().collect();
        for group in &mut self.file_groups {
            group.members.retain(|m| selected_set.contains(m));
        }
        // Remove empty auto-groups (keep manual ones so the user can still add files)
        self.file_groups.retain(|g| g.manual || !g.members.is_empty());
    }

    /// Return the set of file paths that belong to any group.
    fn grouped_paths(&self) -> HashSet<PathBuf> {
        self.file_groups
            .iter()
            .flat_map(|g| g.members.iter().cloned())
            .collect()
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
            self.recompute_groups();
        }
    }

    /// Clear the file list and all results.
    fn clear_all(&mut self) {
        self.selected_files.clear();
        self.results.clear();
        self.group_results.clear();
        self.elapsed_times.clear();
        self.group_elapsed_times.clear();
        self.log_entries.clear();
        self.selection = None;
        self.state = AppState::Idle;
        self.progress = (0, 0);
        self.files_started = 0;
        self.file_groups.clear();
        self.openspec_results.clear();
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
        let group_pairs: Vec<_> = self.group_results.iter().map(|(n, d)| (n.clone(), d.clone())).collect();
        if pairs.is_empty() && group_pairs.is_empty() {
            self.log(LogLevel::Warning, "No generated Gherkin to save.");
            return;
        }
        let count = pairs.len() + group_pairs.len();
        for (path, doc) in &pairs {
            self.save_feature_file(path, doc);
        }
        for (name, doc) in &group_pairs {
            self.save_group_feature_file(name, doc);
        }
        self.log(LogLevel::Success, format!("Saved {} .feature file(s)", count));
    }

    /// Save a group's merged Gherkin document to the output directory.
    fn save_group_feature_file(&mut self, group_name: &str, doc: &GherkinDocument) {
        let dir = match &self.output_dir {
            Some(d) => d.clone(),
            None => {
                self.log(LogLevel::Warning, "No output directory selected. Please choose one first.");
                return;
            }
        };
        let out_path = dir.join(format!("{}.feature", group_name));
        match std::fs::write(&out_path, doc.to_feature_string()) {
            Ok(()) => {
                self.log(LogLevel::Success, format!("Saved: {}", out_path.display()));
                self.toast = Some((format!("Saved {}.feature", group_name), 3.0));
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("Failed to save {}: {}", out_path.display(), e));
            }
        }
    }

    /// Build a `SessionData` snapshot of the current state.
    fn build_session_data(&self) -> crate::session::SessionData {
        // Convert PathBuf-keyed results to String-keyed for serialization
        let results: HashMap<String, GherkinDocument> = self
            .results
            .iter()
            .map(|(p, d)| (p.to_string_lossy().to_string(), d.clone()))
            .collect();
        let previous: HashMap<String, GherkinDocument> = self
            .previous_results
            .iter()
            .map(|(p, d)| (p.to_string_lossy().to_string(), d.clone()))
            .collect();
        crate::session::SessionData {
            files: self.selected_files.clone(),
            groups: self.file_groups.clone(),
            results,
            group_results: self.group_results.clone(),
            ratings: self.ratings.clone(),
            generator_model: self.generator_model.clone(),
            extractor_model: self.extractor_model.clone(),
            reviewer_model: self.reviewer_model.clone(),
            vision_model: self.vision_model.clone(),
            pipeline_mode: self.pipeline_mode,
            max_concurrent: self.max_concurrent,
            output_dir: self.output_dir.clone(),
            previous_results: previous,
            previous_group_results: self.previous_group_results.clone(),
        }
    }

    /// Auto-save session to disk (no-op if no output directory set).
    fn auto_save_session(&mut self) {
        if let Some(dir) = &self.output_dir {
            let data = self.build_session_data();
            if let Err(e) = crate::session::save(dir, &data) {
                self.log(LogLevel::Warning, format!("Session save failed: {}", e));
            }
        }
    }

    /// Restore state from a session file.
    fn restore_session(&mut self, data: crate::session::SessionData) {
        self.selected_files = data.files;
        self.file_groups = data.groups;
        self.results = data
            .results
            .into_iter()
            .map(|(k, v)| (PathBuf::from(k), v))
            .collect();
        self.group_results = data.group_results;
        self.ratings = data.ratings;
        self.generator_model = data.generator_model;
        self.extractor_model = data.extractor_model;
        self.reviewer_model = data.reviewer_model;
        self.vision_model = data.vision_model;
        self.pipeline_mode = data.pipeline_mode;
        self.max_concurrent = data.max_concurrent;
        self.previous_results = data
            .previous_results
            .into_iter()
            .map(|(k, v)| (PathBuf::from(k), v))
            .collect();
        self.previous_group_results = data.previous_group_results;
        if !self.results.is_empty() || !self.group_results.is_empty() {
            self.state = AppState::Done;
        }
        let file_count = self.selected_files.len();
        let result_count = self.results.len() + self.group_results.len();
        self.log(
            LogLevel::Success,
            format!("Session restored: {} files, {} results", file_count, result_count),
        );
        self.toast = Some(("Session restored".to_string(), 3.0));
    }

    /// Get the rating key for the current selection.
    fn selection_rating_key(&self) -> Option<String> {
        match &self.selection {
            Some(Selection::File(idx)) => self
                .selected_files
                .get(*idx)
                .map(|p| p.to_string_lossy().to_string()),
            Some(Selection::Group(name)) => Some(name.clone()),
            None => None,
        }
    }

    /// Kick off background processing for all selected files.
    fn start_processing(&mut self) {
        if self.selected_files.is_empty() {
            self.push_status("⚠ No files selected.");
            return;
        }

        self.state = AppState::Processing;
        // Snapshot current results for diffing after regeneration
        self.previous_results = self.results.clone();
        self.previous_group_results = self.group_results.clone();
        self.results.clear();
        self.group_results.clear();
        self.elapsed_times.clear();
        self.group_elapsed_times.clear();
        self.log_entries.clear();

        // Count work items: each group counts as 1, each ungrouped file counts as 1
        let grouped = self.grouped_paths();
        let ungrouped_count = self
            .selected_files
            .iter()
            .filter(|p| !grouped.contains(*p))
            .count();
        let total_items = self.file_groups.len() + ungrouped_count;

        self.progress = (0, total_items);
        self.files_started = 0;
        if let Ok(mut ctx) = self.context.lock() {
            ctx.clear();
        }

        let (tx, rx): (Sender<ProcessingEvent>, Receiver<ProcessingEvent>) = mpsc::channel();
        self.event_rx = Some(rx);

        let files = self.selected_files.clone();
        let groups = self.file_groups.clone();
        let context = Arc::clone(&self.context);
        let gen_model = self.generator_model.clone();
        let ext_model = self.extractor_model.clone();
        let rev_model = self.reviewer_model.clone();
        let vis_model = self.vision_model.clone();
        let handle = self.runtime.clone();
        let mode = self.pipeline_mode;
        let max_concurrent = self.max_concurrent;
        let openspec_enabled = self.openspec_enabled;
        let openspec_url = self.openspec_url.clone();
        let openspec_output_dir = self.output_dir.clone();
        // Use a LOCAL temp directory for the disk cache so we never do
        // synchronous I/O against a (potentially network-mounted) output dir.
        let local_cache_root = std::env::temp_dir().join("dockock");
        let cache = crate::cache::DiskCache::new(Some(&local_cache_root));
        let force_regenerate = self.force_regenerate;
        let backend = self.backend.clone();

        // Spawn a blocking thread that drives the async work
        std::thread::spawn(move || {
            handle.block_on(process_files(
                files, groups, context, backend,
                gen_model, ext_model, rev_model, vis_model,
                mode,
                max_concurrent, openspec_enabled, openspec_url, openspec_output_dir,
                cache, force_regenerate, tx,
            ));
        });
    }

    /// Kick off a targeted refinement of the currently selected Gherkin output.
    fn start_refinement(&mut self, current_gherkin: String) {
        let instruction = self.refinement_input.trim().to_string();
        if instruction.is_empty() {
            return;
        }
        self.refinement_input.clear();

        let selection = self.selection.clone();
        let model = self.generator_model.clone();
        let handle = self.runtime.clone();
        let backend = self.backend.clone();

        let (tx, rx): (Sender<ProcessingEvent>, Receiver<ProcessingEvent>) = mpsc::channel();
        self.event_rx = Some(rx);
        self.state = AppState::Processing;

        let _ = tx.send(ProcessingEvent::Status(format!(
            "✏ Refining: {}…", instruction
        )));

        std::thread::spawn(move || {
            handle.block_on(async move {
                let preamble = format!(
                    "You are a Gherkin expert. The user has generated the following Gherkin feature file \
                     and wants you to refine it.\n\n\
                     === CURRENT GHERKIN ===\n{}\n\n\
                     === USER INSTRUCTION ===\n{}\n\n\
                     Output ONLY the complete, revised Gherkin feature file. \
                     Keep the Feature title and overall structure. \
                     Apply the user's instruction precisely.",
                    current_gherkin, instruction
                );

                let result = match &backend {
                    crate::llm::ProviderBackend::Ollama => {
                        // Raw HTTP to local Ollama
                        let num_ctx = crate::llm::context_window_for_model(&model);
                        let client = reqwest::Client::new();
                        let resp = client
                            .post(format!("{}/api/generate", crate::llm::ENDPOINT_GENERATOR.url))
                            .json(&serde_json::json!({
                                "model": model,
                                "prompt": preamble,
                                "stream": false,
                                "options": { "num_ctx": num_ctx },
                            }))
                            .send()
                            .await;
                        match resp {
                            Ok(r) => match r.json::<serde_json::Value>().await {
                                Ok(body) => body["response"].as_str().map(|s| s.to_string()),
                                Err(_) => None,
                            },
                            Err(e) => {
                                let _ = tx.send(ProcessingEvent::Status(format!(
                                    "⚠ Refinement failed: {}", e
                                )));
                                None
                            }
                        }
                    }
                    crate::llm::ProviderBackend::Custom { base_url, api_key, .. } => {
                        // OpenAI-compatible API
                        use rig::client::CompletionClient;
                        use rig::completion::Prompt;
                        use rig::providers::openai;

                        match openai::CompletionsClient::builder()
                            .api_key(api_key)
                            .base_url(base_url)
                            .build()
                        {
                            Ok(client) => {
                                let agent = client.agent(&model).build();
                                match agent.prompt(&preamble).await {
                                    Ok(text) => Some(text),
                                    Err(e) => {
                                        let _ = tx.send(ProcessingEvent::Status(format!(
                                            "⚠ Refinement failed: {}", e
                                        )));
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(ProcessingEvent::Status(format!(
                                    "⚠ Failed to create client: {}", e
                                )));
                                None
                            }
                        }
                    }
                };

                if let Some(text) = result {
                    let doc = crate::gherkin::GherkinDocument::parse_from_llm_output(
                        &text,
                        "refinement",
                    );

                    match selection {
                        Some(Selection::File(idx)) => {
                            let _ = tx.send(ProcessingEvent::Status(format!(
                                "✏ Refinement complete (file index {})", idx
                            )));
                            let _ = tx.send(ProcessingEvent::FileResult {
                                path: PathBuf::from(format!("__refine_file_{}", idx)),
                                gherkin: doc,
                                elapsed: std::time::Duration::ZERO,
                            });
                        }
                        Some(Selection::Group(name)) => {
                            let _ = tx.send(ProcessingEvent::GroupResult {
                                group_name: name,
                                gherkin: doc,
                                elapsed: std::time::Duration::ZERO,
                            });
                        }
                        None => {}
                    }
                } else if result.is_none() {
                    let _ = tx.send(ProcessingEvent::Status(
                        "⚠ Refinement: no response from LLM".to_string(),
                    ));
                }

                let _ = tx.send(ProcessingEvent::Done(Ok(())));
                let _ = tx.send(ProcessingEvent::OpenSpecDone(Ok(0)));
            });
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
                ProcessingEvent::FileResult { path, gherkin, elapsed } => {
                    // Handle refinement results with synthetic path markers
                    let actual_path = if let Some(idx_str) = path.to_string_lossy().strip_prefix("__refine_file_") {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            self.selected_files.get(idx).cloned().unwrap_or(path.clone())
                        } else {
                            path.clone()
                        }
                    } else {
                        path.clone()
                    };

                    self.progress.0 += 1;
                    let secs = elapsed.as_secs_f64();
                    let elapsed_str = if secs >= 60.0 {
                        format!("{:.0}m {:.0}s", (secs / 60.0).floor(), secs % 60.0)
                    } else {
                        format!("{:.1}s", secs)
                    };
                    self.log(LogLevel::Success, format!(
                        "✓ Generated Gherkin for: {} ({}/{}) in {}",
                        actual_path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        self.progress.0,
                        self.progress.1,
                        elapsed_str,
                    ));
                    self.elapsed_times.insert(actual_path.clone(), elapsed);
                    self.results.insert(actual_path, gherkin);
                }
                ProcessingEvent::GroupResult { group_name, gherkin, elapsed } => {
                    self.progress.0 += 1;
                    let secs = elapsed.as_secs_f64();
                    let elapsed_str = if secs >= 60.0 {
                        format!("{:.0}m {:.0}s", (secs / 60.0).floor(), secs % 60.0)
                    } else {
                        format!("{:.1}s", secs)
                    };
                    self.log(LogLevel::Success, format!(
                        "✓ Generated Gherkin for group: {} ({}/{}) in {}",
                        group_name,
                        self.progress.0,
                        self.progress.1,
                        elapsed_str,
                    ));
                    self.group_elapsed_times.insert(group_name.clone(), elapsed);
                    self.group_results.insert(group_name, gherkin);
                }
                ProcessingEvent::Done(result) => {
                    match result {
                        Ok(()) => {
                            self.log(LogLevel::Success, format!(
                                "✅ All {} files processed successfully.",
                                self.progress.1
                            ));
                            self.auto_save_session();
                        }
                        Err(e) => {
                            self.log(LogLevel::Error, format!("❌ Processing failed: {}", e));
                            self.event_rx = None;
                            self.state = AppState::Done;
                        }
                    }
                }
                ProcessingEvent::OpenSpecStarted => {
                    self.log(LogLevel::Info, "📦 Starting OpenSpec export phase…");
                }
                ProcessingEvent::OpenSpecResult { change_name, result } => {
                    self.log(LogLevel::Success, format!(
                        "📦 OpenSpec exported: {} ({} artifacts)",
                        change_name,
                        result.artifacts.len()
                    ));
                    self.openspec_results.insert(change_name, result);
                }
                ProcessingEvent::OpenSpecDone(result) => {
                    self.event_rx = None;
                    self.state = AppState::Done;
                    match result {
                        Ok(count) => {
                            self.log(LogLevel::Success, format!(
                                "✅ Processing complete. {} OpenSpec export(s) saved.", count
                            ));
                            self.toast = Some(("Processing complete!".to_string(), 4.0));
                        }
                        Err(e) => {
                            self.log(LogLevel::Error, format!("❌ OpenSpec export failed: {}", e));
                            self.toast = Some(("Processing complete (OpenSpec errors)".to_string(), 4.0));
                        }
                    }
                    self.auto_save_session();
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
        // ── Row 1: Title, Ollama status, Output directory ──
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
                    self.output_dir = Some(dir.clone());
                    if crate::session::exists(&dir) {
                        self.session_restore_pending = true;
                    }
                }
            }
            ui.separator();
            ui.checkbox(&mut self.force_regenerate, "🔄 Force")
                .on_hover_text("Force re-generation, bypassing cache");
            ui.checkbox(&mut self.openspec_enabled, "📦 OpenSpec");
            if self.openspec_enabled {
                match self.openspec_ok {
                    Some(true) => { ui.colored_label(egui::Color32::GREEN, "●"); }
                    Some(false) => { ui.colored_label(egui::Color32::RED, "●"); }
                    None => { ui.colored_label(egui::Color32::GRAY, "○"); }
                }
            }
        });

        ui.add_space(2.0);

        // ── Row 2: Provider, Models, Pipeline, Concurrency ──
        ui.horizontal(|ui| {
            // Provider selector
            ui.label("Provider:");
            let current_label = self.backend.display_name().to_string();
            let was_custom = self.backend.is_custom();
            egui::ComboBox::from_id_salt("provider_backend")
                .selected_text(&current_label)
                .width(140.0)
                .show_ui(ui, |ui| {
                    // Ollama option
                    if ui.selectable_label(
                        matches!(self.backend, crate::llm::ProviderBackend::Ollama),
                        "Ollama (local)",
                    ).clicked() {
                        self.backend = crate::llm::ProviderBackend::Ollama;
                    }
                    // Custom providers from JSON — collect names to avoid borrow conflict
                    let provider_names: Vec<String> = self.custom_providers.iter()
                        .map(|c| c.name.clone())
                        .collect();
                    for pname in &provider_names {
                        let is_selected = match &self.backend {
                            crate::llm::ProviderBackend::Custom { name, .. } => name == pname,
                            _ => false,
                        };
                        if ui.selectable_label(is_selected, pname).clicked() {
                            if let Some(be) = crate::llm::build_custom_backend(&self.custom_providers) {
                                self.backend = be;
                            } else {
                                self.log(LogLevel::Warning, format!(
                                    "No API key found for provider '{}'. Set the env var and restart.",
                                    pname
                                ));
                            }
                        }
                    }
                });

            // Auto-assign models when provider changes
            let is_custom = self.backend.is_custom();
            if is_custom && !was_custom {
                // Switching TO custom — save Ollama models & apply defaults
                self.saved_ollama_models = (
                    self.generator_model.clone(),
                    self.extractor_model.clone(),
                    self.reviewer_model.clone(),
                    self.vision_model.clone(),
                );
                if let Some(cfg) = self.custom_providers.first() {
                    let models: Vec<String> = cfg.models.keys().cloned().collect();
                    let first = models.first().cloned().unwrap_or_default();
                    self.generator_model = cfg.defaults.generator.clone().unwrap_or_else(|| first.clone());
                    self.extractor_model = cfg.defaults.extractor.clone().unwrap_or_else(|| first.clone());
                    self.reviewer_model = cfg.defaults.reviewer.clone().unwrap_or_else(|| first.clone());
                    self.vision_model = cfg.defaults.vision.clone().unwrap_or(first);
                }
            } else if !is_custom && was_custom {
                // Switching BACK to Ollama — restore saved models
                self.generator_model = self.saved_ollama_models.0.clone();
                self.extractor_model = self.saved_ollama_models.1.clone();
                self.reviewer_model = self.saved_ollama_models.2.clone();
                self.vision_model = self.saved_ollama_models.3.clone();
            }

            // API key indicator for custom providers
            if self.backend.is_custom() {
                ui.colored_label(egui::Color32::GREEN, "🔑");
            }

            ui.separator();
            ui.label("Models ─");

            // Gen/Ext/Rev use custom models when custom provider selected
            let custom_models: Vec<String> = if self.backend.is_custom() {
                crate::llm::custom_model_ids(&self.custom_providers)
            } else {
                Vec::new()
            };

            ui.label("Gen:");
            if self.backend.is_custom() {
                custom_model_combo(ui, "gen_model", &mut self.generator_model, &custom_models);
            } else {
                model_combo(ui, "gen_model", &mut self.generator_model);
            }
            ui.label("Ext:");
            if self.backend.is_custom() {
                custom_model_combo(ui, "ext_model", &mut self.extractor_model, &custom_models);
            } else {
                model_combo(ui, "ext_model", &mut self.extractor_model);
            }
            ui.label("Rev:");
            if self.backend.is_custom() {
                custom_model_combo(ui, "rev_model", &mut self.reviewer_model, &custom_models);
            } else {
                model_combo(ui, "rev_model", &mut self.reviewer_model);
            }

            // Vision uses cloud models when custom provider selected, local otherwise
            ui.label("Vis:");
            if self.backend.is_custom() {
                custom_model_combo(ui, "vis_model", &mut self.vision_model, &custom_models);
            } else {
                model_combo(ui, "vis_model", &mut self.vision_model);
            }
            ui.separator();
            ui.label("Pipeline:");
            egui::ComboBox::from_id_salt("pipeline_mode")
                .selected_text(self.pipeline_mode.to_string())
                .show_ui(ui, |ui| {
                    for mode in crate::llm::PipelineMode::ALL {
                        ui.selectable_value(&mut self.pipeline_mode, mode, mode.to_string());
                    }
                });
            ui.label("∥");
            ui.add(egui::DragValue::new(&mut self.max_concurrent).range(1..=8).speed(0.1))
                .on_hover_text("Max concurrent LLM tasks");
        });
    }

    fn render_left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("📂 Files");
        ui.separator();

        let is_processing = self.state == AppState::Processing;

        // ── Toolbar ──
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
            if ui
                .add_enabled(!is_processing, egui::Button::new("📎 New Group"))
                .clicked()
            {
                self.show_new_group_input = !self.show_new_group_input;
                self.new_group_name.clear();
            }
        });

        // Auto-group toggle
        ui.horizontal(|ui| {
            let prev = self.auto_group_enabled;
            ui.checkbox(&mut self.auto_group_enabled, "Auto-group by name");
            if self.auto_group_enabled != prev {
                self.recompute_groups();
            }
        });

        // New group name input
        if self.show_new_group_input && !is_processing {
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut self.new_group_name);
                if ui.button("✔").clicked() && !self.new_group_name.trim().is_empty() {
                    let name = self.new_group_name.trim().to_string();
                    if !self.file_groups.iter().any(|g| g.name == name) {
                        self.file_groups.push(FileGroup {
                            name,
                            members: Vec::new(),
                            manual: true,
                        });
                    }
                    self.new_group_name.clear();
                    self.show_new_group_input = false;
                }
                if ui.button("✖").clicked() {
                    self.show_new_group_input = false;
                    self.new_group_name.clear();
                }
            });
        }

        // Progress bar during processing
        if self.state == AppState::Processing && self.progress.1 > 0 {
            ui.add_space(4.0);
            let completed = self.progress.0 as f32;
            let started = self.files_started.saturating_sub(self.progress.0) as f32;
            let total = self.progress.1 as f32;
            let fraction = ((completed + started * 0.5) / total).clamp(0.0, 1.0);
            let bar = egui::ProgressBar::new(fraction)
                .text(format!("{}/{} items", self.progress.0, self.progress.1))
                .animate(true);
            ui.add(bar);
        }

        ui.add_space(4.0);

        // ── File list with groups ──
        let grouped_paths = self.grouped_paths();
        let group_names: Vec<String> = self.file_groups.iter().map(|g| g.name.clone()).collect();

        egui::ScrollArea::vertical()
            .id_salt("file_list")
            .max_height(ui.available_height() - 60.0)
            .show(ui, |ui| {
                // Deferred actions to apply after iteration
                let mut remove_file: Option<usize> = None;
                let mut remove_from_group: Option<(usize, usize)> = None; // (group_idx, member_idx)
                let mut delete_group: Option<usize> = None;
                let mut move_to_group: Option<(PathBuf, usize)> = None; // (path, group_idx)

                // ── Render groups ──
                for (gi, group) in self.file_groups.iter().enumerate() {
                    let group_selected = self.selection == Some(Selection::Group(group.name.clone()));
                    let has_group_result = self.group_results.contains_key(&group.name);

                    let header_label = if has_group_result {
                        if let Some(dur) = self.group_elapsed_times.get(&group.name) {
                            let secs = dur.as_secs_f64();
                            let elapsed_str = if secs >= 60.0 {
                                format!("{:.0}m {:.0}s", (secs / 60.0).floor(), secs % 60.0)
                            } else {
                                format!("{:.1}s", secs)
                            };
                            format!("✓ 📎 {} ({} files) ({})", group.name, group.members.len(), elapsed_str)
                        } else {
                            format!("✓ 📎 {} ({} files)", group.name, group.members.len())
                        }
                    } else {
                        format!("📎 {} ({} files)", group.name, group.members.len())
                    };

                    let id = ui.make_persistent_id(format!("group_{}", gi));
                    egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        id,
                        true,
                    )
                    .show_header(ui, |ui| {
                        let resp = ui.selectable_label(group_selected, &header_label);
                        if resp.clicked() {
                            self.selection = Some(Selection::Group(group.name.clone()));
                        }
                        if !is_processing {
                            if ui.small_button("✖").on_hover_text("Delete group").clicked() {
                                delete_group = Some(gi);
                            }
                        }
                    })
                    .body(|ui| {
                        for (mi, member) in group.members.iter().enumerate() {
                            let name = member
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            ui.horizontal(|ui| {
                                ui.label(format!("   {}", name));
                                if !is_processing {
                                    if ui.small_button("✖").on_hover_text("Remove from group").clicked() {
                                        remove_from_group = Some((gi, mi));
                                    }
                                }
                            });
                        }
                        // ── Add file to group button ──
                        if !is_processing {
                            let ungrouped: Vec<(PathBuf, String)> = self
                                .selected_files
                                .iter()
                                .filter(|p| !grouped_paths.contains(*p))
                                .map(|p| {
                                    let n = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| p.to_string_lossy().to_string());
                                    (p.clone(), n)
                                })
                                .collect();
                            if !ungrouped.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    ui.menu_button("➕ Add file…", |ui| {
                                        for (path, fname) in &ungrouped {
                                            if ui.button(fname).clicked() {
                                                move_to_group = Some((path.clone(), gi));
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                            }
                        }
                    });
                }

                // ── Render ungrouped files ──
                if self.selected_files.iter().any(|p| !grouped_paths.contains(p)) {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.label(egui::RichText::new("Ungrouped").italics());
                }

                for (i, path) in self.selected_files.iter().enumerate() {
                    if grouped_paths.contains(path) {
                        continue;
                    }
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());

                    let has_result = self.results.contains_key(path);
                    let label = if has_result {
                        if let Some(dur) = self.elapsed_times.get(path) {
                            let secs = dur.as_secs_f64();
                            let elapsed_str = if secs >= 60.0 {
                                format!("{:.0}m {:.0}s", (secs / 60.0).floor(), secs % 60.0)
                            } else {
                                format!("{:.1}s", secs)
                            };
                            format!("✓ {} ({})", name, elapsed_str)
                        } else {
                            format!("✓ {}", name)
                        }
                    } else {
                        name.clone()
                    };

                    let selected = self.selection == Some(Selection::File(i));
                    let resp = ui
                        .selectable_label(selected, &label)
                        .on_hover_text(path.to_string_lossy().as_ref());

                    if resp.clicked() {
                        self.selection = Some(Selection::File(i));
                    }

                    if !is_processing {
                        resp.context_menu(|ui| {
                            if ui.button("Remove").clicked() {
                                remove_file = Some(i);
                                ui.close();
                            }
                            if !group_names.is_empty() {
                                ui.menu_button("Move to group…", |ui| {
                                    for (gi, gname) in group_names.iter().enumerate() {
                                        if ui.button(gname).clicked() {
                                            move_to_group = Some((path.clone(), gi));
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        });
                    }
                }

                // ── Apply deferred mutations ──
                if let Some(idx) = remove_file {
                    let path = self.selected_files.remove(idx);
                    self.results.remove(&path);
                    self.elapsed_times.remove(&path);
                    if self.selection == Some(Selection::File(idx)) {
                        self.selection = None;
                    }
                    self.recompute_groups();
                }

                if let Some((gi, mi)) = remove_from_group {
                    if let Some(group) = self.file_groups.get_mut(gi) {
                        group.members.remove(mi);
                        if group.members.is_empty() && !group.manual {
                            self.file_groups.remove(gi);
                        }
                    }
                }

                if let Some(gi) = delete_group {
                    if gi < self.file_groups.len() {
                        let removed_name = self.file_groups[gi].name.clone();
                        self.file_groups.remove(gi);
                        self.group_results.remove(&removed_name);
                        self.group_elapsed_times.remove(&removed_name);
                        if self.selection == Some(Selection::Group(removed_name)) {
                            self.selection = None;
                        }
                    }
                }

                if let Some((path, gi)) = move_to_group {
                    // Remove from any existing group first
                    for group in &mut self.file_groups {
                        group.members.retain(|m| m != &path);
                    }
                    if let Some(group) = self.file_groups.get_mut(gi) {
                        if !group.members.contains(&path) {
                            group.members.push(path);
                        }
                    }
                    // Remove empty auto-groups (keep manual ones)
                    self.file_groups.retain(|g| g.manual || !g.members.is_empty());
                }
            });
    }

    fn render_right_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("📝 Gherkin Output");
        ui.separator();

        let content = match &self.selection {
            Some(Selection::File(idx)) => self
                .selected_files
                .get(*idx)
                .and_then(|p| self.results.get(p))
                .map(|doc| doc.to_feature_string()),
            Some(Selection::Group(name)) => self
                .group_results
                .get(name)
                .map(|doc| doc.to_feature_string()),
            None => None,
        };

        // Check for previous result (for diff view)
        let prev_content = match &self.selection {
            Some(Selection::File(idx)) => self
                .selected_files
                .get(*idx)
                .and_then(|p| self.previous_results.get(p))
                .map(|doc| doc.to_feature_string()),
            Some(Selection::Group(name)) => self
                .previous_group_results
                .get(name)
                .map(|doc| doc.to_feature_string()),
            None => None,
        };

        let has_diff = prev_content.is_some() && content.is_some();

        match content {
            Some(text) => {
                // ── Button bar ──
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
                        match &self.selection {
                            Some(Selection::File(idx)) => {
                                if let Some(path) = self.selected_files.get(*idx).cloned() {
                                    if let Some(doc) = self.results.get(&path).cloned() {
                                        self.save_feature_file(&path, &doc);
                                    }
                                }
                            }
                            Some(Selection::Group(name)) => {
                                if let Some(doc) = self.group_results.get(name).cloned() {
                                    let name = name.clone();
                                    self.save_group_feature_file(&name, &doc);
                                }
                            }
                            None => {}
                        }
                    }
                    if ui
                        .add_enabled(can_save && (!self.results.is_empty() || !self.group_results.is_empty()), egui::Button::new("💾 Save All"))
                        .on_hover_text(if can_save { "Save all .feature files" } else { "Set output directory first" })
                        .clicked()
                    {
                        self.save_all_feature_files();
                    }

                    ui.separator();

                    // ── Rating buttons ──
                    let rating_key = self.selection_rating_key();
                    if let Some(ref key) = rating_key {
                        let current_rating = self.ratings.get(key).copied();
                        let up_color = if current_rating == Some(crate::session::Rating::ThumbsUp) {
                            egui::Color32::from_rgb(80, 200, 80)
                        } else {
                            egui::Color32::from_rgb(160, 160, 160)
                        };
                        let down_color = if current_rating == Some(crate::session::Rating::ThumbsDown) {
                            egui::Color32::from_rgb(220, 80, 80)
                        } else {
                            egui::Color32::from_rgb(160, 160, 160)
                        };
                        if ui
                            .button(egui::RichText::new("👍").color(up_color))
                            .on_hover_text("Rate as good")
                            .clicked()
                        {
                            let key = key.clone();
                            if current_rating == Some(crate::session::Rating::ThumbsUp) {
                                self.ratings.remove(&key);
                            } else {
                                self.ratings.insert(key, crate::session::Rating::ThumbsUp);
                            }
                            self.auto_save_session();
                        }
                        if ui
                            .button(egui::RichText::new("👎").color(down_color))
                            .on_hover_text("Rate as needs improvement")
                            .clicked()
                        {
                            let key = key.clone();
                            if current_rating == Some(crate::session::Rating::ThumbsDown) {
                                self.ratings.remove(&key);
                            } else {
                                self.ratings.insert(key, crate::session::Rating::ThumbsDown);
                            }
                            self.auto_save_session();
                        }
                    }

                    // ── Diff toggle ──
                    if has_diff {
                        ui.separator();
                        let diff_label = if self.show_diff { "📊 Hide Diff" } else { "📊 Show Diff" };
                        if ui.button(diff_label).on_hover_text("Compare with previous generation").clicked() {
                            self.show_diff = !self.show_diff;
                        }
                    }
                });
                ui.add_space(4.0);

                // ── Main content area ──
                egui::ScrollArea::vertical()
                    .id_salt("gherkin_scroll")
                    .show(ui, |ui| {
                        // Show diff view or normal view
                        if self.show_diff && has_diff {
                            if let Some(ref prev) = prev_content {
                                let diff = crate::session::diff_gherkin(prev, &text);
                                for line in &diff {
                                    match line {
                                        crate::session::DiffLine::Unchanged(s) => {
                                            ui.monospace(s);
                                        }
                                        crate::session::DiffLine::Added(s) => {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(80, 200, 80),
                                                egui::RichText::new(format!("+ {}", s)).monospace(),
                                            );
                                        }
                                        crate::session::DiffLine::Removed(s) => {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(220, 80, 80),
                                                egui::RichText::new(format!("- {}", s)).monospace(),
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            ui.add(
                                egui::TextEdit::multiline(&mut text.as_str())
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                        }

                        // ── OpenSpec artifacts (if available) ──
                        let openspec_key = match &self.selection {
                            Some(Selection::File(idx)) => self
                                .selected_files
                                .get(*idx)
                                .and_then(|p| p.file_stem())
                                .map(|s| s.to_string_lossy().to_string()),
                            Some(Selection::Group(name)) => Some(name.clone()),
                            None => None,
                        };
                        if let Some(key) = openspec_key {
                            if let Some(export) = self.openspec_results.get(&key) {
                                ui.add_space(12.0);
                                ui.separator();
                                ui.heading("📦 OpenSpec Artifacts");
                                ui.add_space(4.0);

                                let mut artifact_names: Vec<&String> =
                                    export.artifacts.keys().collect();
                                artifact_names.sort();

                                for name in artifact_names {
                                    if let Some(content) = export.artifacts.get(name) {
                                        egui::CollapsingHeader::new(
                                            egui::RichText::new(format!("📄 {}", name)).strong(),
                                        )
                                        .default_open(false)
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::TextEdit::multiline(
                                                    &mut content.as_str(),
                                                )
                                                .font(egui::TextStyle::Monospace)
                                                .desired_width(f32::INFINITY),
                                            );
                                        });
                                    }
                                }
                            }
                        }
                    });

                // ── Iterative refinement input ──
                ui.add_space(4.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("✏ Refine:");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.refinement_input)
                            .hint_text("e.g. 'Add error handling scenarios', 'Make steps more specific'")
                            .desired_width(ui.available_width() - 80.0),
                    );
                    let can_refine = !self.refinement_input.trim().is_empty()
                        && self.state != AppState::Processing;
                    if ui.add_enabled(can_refine, egui::Button::new("▶ Apply")).clicked()
                        || (response.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && can_refine)
                    {
                        self.start_refinement(text);
                    }
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

        // Session restore prompt
        if self.session_restore_pending {
            egui::Window::new("Restore Session?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("A previous session was found in this directory.");
                    ui.label("Would you like to restore it?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("✔ Restore").clicked() {
                            if let Some(dir) = &self.output_dir {
                                if let Some(data) = crate::session::load(dir) {
                                    self.restore_session(data);
                                }
                            }
                            self.session_restore_pending = false;
                        }
                        if ui.button("✖ Skip").clicked() {
                            self.session_restore_pending = false;
                        }
                    });
                });
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
/// Groups of related files produce a single merged Gherkin output each.
async fn process_files(
    files: Vec<PathBuf>,
    groups: Vec<FileGroup>,
    context: Arc<Mutex<ProjectContext>>,
    backend: crate::llm::ProviderBackend,
    generator_model: String,
    extractor_model: String,
    reviewer_model: String,
    vision_model: String,
    mode: crate::llm::PipelineMode,
    max_concurrent: usize,
    openspec_enabled: bool,
    openspec_url: String,
    openspec_output_dir: Option<PathBuf>,
    cache: crate::cache::DiskCache,
    force_regenerate: bool,
    tx: Sender<ProcessingEvent>,
) {
    let total = files.len();

    // ── Phase 0: Spin up the orchestrator and probe all Ollama instances ──
    let _ = tx.send(ProcessingEvent::Status(
        "🔌 Probing Ollama instances…".to_string(),
    ));

    let (orchestrator, statuses) = match crate::llm::AgentOrchestrator::new(
        backend.clone(),
        &generator_model,
        &extractor_model,
        &reviewer_model,
        &vision_model,
        mode,
        max_concurrent,
        cache.clone(),
    ).await {
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

    // Warm up all models in parallel (forces model loading, eliminates cold-start)
    // Run warm-up concurrently with file parsing to overlap I/O.
    let _ = tx.send(ProcessingEvent::Status(
        "🔥 Warming up models…".to_string(),
    ));
    let orchestrator = Arc::new(orchestrator);
    let warmup_orch = Arc::clone(&orchestrator);
    let warmup_handle = tokio::spawn(async move {
        warmup_orch.warm_up().await;
    });

    // ── Phase 1: Parse ALL files in parallel (CPU/IO bound, no LLM) ──
    // Runs concurrently with model warm-up above.
    let _ = tx.send(ProcessingEvent::Status(format!(
        "📄 Parsing {} files in parallel…", total
    )));

    let mut parse_handles = Vec::with_capacity(total);
    for path in &files {
        let p = path.clone();
        let cache = cache.clone();
        parse_handles.push(tokio::task::spawn_blocking(move || {
            // Check parsed file cache first
            let file_bytes = std::fs::read(&p).ok();
            let cache_key = file_bytes
                .as_ref()
                .map(|b| crate::cache::content_hash(b));

            if let Some(ref key) = cache_key {
                if let Some(cached) = cache.get::<crate::parser::ParseResult>(crate::cache::NS_PARSED, key) {
                    return Ok((p, cached, true));
                }
            }

            crate::parser::parse_file(&p).map(|r| {
                // Store in cache
                if let Some(ref key) = cache_key {
                    cache.put(crate::cache::NS_PARSED, key, &r);
                }
                (p, r, false)
            })
        }));
    }

    // Collect parsed results into a lookup
    let mut parsed_map: HashMap<PathBuf, (String, String, Vec<crate::parser::ExtractedImage>)> =
        HashMap::new();
    let mut cache_hits = 0usize;
    for handle in parse_handles {
        match handle.await {
            Ok(Ok((path, result, from_cache))) => {
                let name = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let img_count = result.images.len();
                let cache_label = if from_cache { " (cached)" } else { "" };
                if from_cache { cache_hits += 1; }
                let _ = tx.send(ProcessingEvent::Status(format!(
                    "📄 Parsed: {} ({} images found){}", name, img_count, cache_label
                )));

                // Store in shared context
                {
                    let content = crate::context::FileContent {
                        path: path.clone(),
                        file_type: result.file_type.clone(),
                        raw_text: result.text.clone(),
                    };
                    if let Ok(mut ctx) = context.lock() {
                        ctx.add_file(content);
                    }
                }

                parsed_map.insert(path, (result.file_type, result.text, result.images));
            }
            Ok(Err(e)) => {
                let _ = tx.send(ProcessingEvent::Status(format!("⚠ Parse error: {}", e)));
            }
            Err(e) => {
                let _ = tx.send(ProcessingEvent::Status(format!("⚠ Parse task panicked: {}", e)));
            }
        }
    }

    if cache_hits > 0 {
        let _ = tx.send(ProcessingEvent::Status(format!(
            "📦 Cache: {}/{} files loaded from cache", cache_hits, total
        )));
    }

    let _ = tx.send(ProcessingEvent::Status(format!(
        "✅ Parsed {}/{} files. Starting multi-agent pipeline…",
        parsed_map.len(),
        total
    )));

    // ── Phase 1.25: Extract entity glossary from all parsed content ──
    {
        if let Ok(mut ctx) = context.lock() {
            ctx.extract_entities();
            let entity_count = ctx.entities.len();
            if entity_count > 0 {
                let _ = tx.send(ProcessingEvent::Status(format!(
                    "🏷 Extracted {} entities for project glossary", entity_count
                )));
            }
        }
    }

    // Ensure model warm-up has finished before starting LLM tasks
    let _ = warmup_handle.await;
    let _ = tx.send(ProcessingEvent::Status(
        "✅ Models warmed up.".to_string(),
    ));

    // ── Phase 1.35: Prime the generator KV-cache with the shared prefix ──
    {
        let glossary = if let Ok(ctx) = context.lock() {
            ctx.build_glossary()
        } else {
            String::new()
        };
        if !glossary.is_empty() {
            let _ = tx.send(ProcessingEvent::Status(
                "⚡ Priming generator KV-cache prefix…".to_string(),
            ));
            let _ = orchestrator
                .prime_generator_prefix(crate::llm::GENERATOR_PREAMBLE, &glossary)
                .await;
            let _ = tx.send(ProcessingEvent::Status(
                "⚡ Generator prefix cache ready.".to_string(),
            ));
        }
    }

    // ── Phase 1.5: Build work items (groups vs ungrouped singles) ──
    let grouped_paths: std::collections::HashSet<PathBuf> = groups
        .iter()
        .flat_map(|g| g.members.iter().cloned())
        .collect();

    // ── Phase 1.3: RAG disabled ──
    // RAG cross-file context has been removed to avoid runtime hangs.
    // The pipeline uses excerpt-based context instead.

    // Take a snapshot of context now (after all files are parsed)
    let ctx_snapshot = context.lock().map(|c| c.clone()).unwrap_or_default();

    let mut llm_handles = Vec::new();

    // Shared collection for OpenSpec: (change_name, gherkin_feature_text)
    let gherkin_docs: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let _ = tx.send(ProcessingEvent::Status(format!(
        "🔧 Dispatching {} groups + ungrouped files (concurrency: {})…",
        groups.len(), max_concurrent
    )));

    // ── Dispatch group work items ──
    for group in &groups {
        // Collect parsed data for each member
        let mut members_data: Vec<(String, String, String, Vec<crate::parser::ExtractedImage>)> =
            Vec::new();
        for member_path in &group.members {
            if let Some((file_type, text, images)) = parsed_map.get(member_path) {
                let fname = member_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                members_data.push((fname, file_type.clone(), text.clone(), images.clone()));
            }
        }

        if members_data.is_empty() {
            continue;
        }

        // If group has only 1 member, treat as a single file
        if members_data.len() == 1 {
            let member_path = group.members[0].clone();
            let (file_name, file_type, raw_text, images) = members_data.into_iter().next().unwrap();
            let orch = Arc::clone(&orchestrator);
            let sem = Arc::clone(&orchestrator.semaphore);
            let tx = tx.clone();
            let ctx = ctx_snapshot.clone();
            let gdocs = Arc::clone(&gherkin_docs);
            let force_regen = force_regenerate;

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let _ = tx.send(ProcessingEvent::FileStarted(member_path.clone()));
                let file_start = std::time::Instant::now();

                let rag_ctx: Option<String> = None;

                let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
                let tx_fwd = tx.clone();
                let fwd = std::thread::spawn(move || {
                    while let Ok(msg) = status_rx.recv() {
                        let _ = tx_fwd.send(ProcessingEvent::Status(msg));
                    }
                });

                let result = orch
                    .process_file(&file_name, &file_type, &raw_text, &images, &ctx, rag_ctx.as_deref(), &status_tx, force_regen)
                    .await;

                drop(status_tx);
                let _ = fwd.join();

                let elapsed = file_start.elapsed();
                match result {
                    Ok(raw_gherkin) => {
                        let doc = crate::gherkin::GherkinDocument::parse_from_llm_output(
                            &raw_gherkin,
                            &file_name,
                        );
                        let feature_text = doc.to_feature_string();
                        if let Ok(mut docs) = gdocs.lock() {
                            let stem = member_path.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| file_name.clone());
                            docs.push((stem, feature_text));
                        }
                        let _ = tx.send(ProcessingEvent::FileResult {
                            path: member_path,
                            gherkin: doc,
                            elapsed,
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
            continue;
        }

        let group_name = group.name.clone();
        let member_paths = group.members.clone();
        let orch = Arc::clone(&orchestrator);
        let sem = Arc::clone(&orchestrator.semaphore);
        let tx = tx.clone();
        let ctx = ctx_snapshot.clone();
        let gdocs = Arc::clone(&gherkin_docs);
        let force_regen = force_regenerate;

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await;

            // Use the first member path for FileStarted signal
            if let Some(first) = member_paths.first() {
                let _ = tx.send(ProcessingEvent::FileStarted(first.clone()));
            }
            let group_start = std::time::Instant::now();

            let rag_ctx: Option<String> = None;

            let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
            let tx_fwd = tx.clone();
            let fwd = std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    let _ = tx_fwd.send(ProcessingEvent::Status(msg));
                }
            });

            let members_ref: Vec<(String, String, String, Vec<crate::parser::ExtractedImage>)> =
                members_data;

            let result = orch
                .process_group(&group_name, &members_ref, &ctx, rag_ctx.as_deref(), &status_tx, force_regen)
                .await;

            drop(status_tx);
            let _ = fwd.join();

            let elapsed = group_start.elapsed();
            match result {
                Ok(raw_gherkin) => {
                    let doc = crate::gherkin::GherkinDocument::parse_from_llm_output(
                        &raw_gherkin,
                        &group_name,
                    );
                    let feature_text = doc.to_feature_string();
                    if let Ok(mut docs) = gdocs.lock() {
                        docs.push((group_name.clone(), feature_text));
                    }
                    let _ = tx.send(ProcessingEvent::GroupResult {
                        group_name,
                        gherkin: doc,
                        elapsed,
                    });
                }
                Err(e) => {
                    let _ = tx.send(ProcessingEvent::Status(format!(
                        "⚠ Pipeline error for group {}: {}",
                        group_name, e
                    )));
                }
            }
        });
        llm_handles.push(handle);
    }

    let _ = tx.send(ProcessingEvent::Status(format!(
        "🔧 Dispatched {} group tasks, now spawning ungrouped file tasks…",
        llm_handles.len()
    )));

    // ── Dispatch ungrouped single-file work items ──
    for (path, (file_type, raw_text, images)) in &parsed_map {
        if grouped_paths.contains(path) {
            continue;
        }

        let path = path.clone();
        let file_type = file_type.clone();
        let raw_text = raw_text.clone();
        let images = images.clone();
        let orch = Arc::clone(&orchestrator);
        let sem = Arc::clone(&orchestrator.semaphore);
        let tx = tx.clone();
        let ctx = ctx_snapshot.clone();
        let gdocs = Arc::clone(&gherkin_docs);
        let force_regen = force_regenerate;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let _ = tx.send(ProcessingEvent::FileStarted(path.clone()));
            let _ = tx.send(ProcessingEvent::Status(format!(
                "🚀 Starting pipeline for {}…", file_name
            )));
            let file_start = std::time::Instant::now();

            let rag_ctx: Option<String> = None;

            let (status_tx, status_rx) = std::sync::mpsc::channel::<String>();
            let tx_fwd = tx.clone();
            let fwd = std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    let _ = tx_fwd.send(ProcessingEvent::Status(msg));
                }
            });

            let result = orch
                .process_file(&file_name, &file_type, &raw_text, &images, &ctx, rag_ctx.as_deref(), &status_tx, force_regen)
                .await;

            drop(status_tx);
            let _ = fwd.join();

            let file_elapsed = file_start.elapsed();
            match result {
                Ok(raw_gherkin) => {
                    let doc = crate::gherkin::GherkinDocument::parse_from_llm_output(
                        &raw_gherkin,
                        &file_name,
                    );
                    let feature_text = doc.to_feature_string();
                    if let Ok(mut docs) = gdocs.lock() {
                        let stem = path.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| file_name.clone());
                        docs.push((stem, feature_text));
                    }
                    let _ = tx.send(ProcessingEvent::FileResult {
                        path: path.clone(),
                        gherkin: doc,
                        elapsed: file_elapsed,
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

    let _ = tx.send(ProcessingEvent::Status(format!(
        "⏳ All {} tasks spawned — waiting for LLM results…",
        llm_handles.len()
    )));

    // Wait for all LLM tasks to complete
    for handle in llm_handles {
        let _ = handle.await;
    }

    let _ = tx.send(ProcessingEvent::Done(Ok(())));

    // ── Phase 3 (optional): OpenSpec export ──
    if !openspec_enabled {
        let _ = tx.send(ProcessingEvent::OpenSpecDone(Ok(0)));
        return;
    }

    let _ = tx.send(ProcessingEvent::OpenSpecStarted);

    // Check service availability
    if let Err(e) = crate::openspec::check_service(&openspec_url).await {
        let _ = tx.send(ProcessingEvent::Status(format!("⚠ {}", e)));
        let _ = tx.send(ProcessingEvent::OpenSpecDone(Err(e)));
        return;
    }

    // Retrieve collected Gherkin docs
    let docs = gherkin_docs.lock().map(|d| d.clone()).unwrap_or_default();
    if docs.is_empty() {
        let _ = tx.send(ProcessingEvent::OpenSpecDone(Ok(0)));
        return;
    }

    if openspec_output_dir.is_none() {
        let _ = tx.send(ProcessingEvent::Status(
            "⚠ No output directory set — OpenSpec artifacts will only be available in-app, not saved to disk.".to_string()
        ));
    }

    let mut ok_count = 0usize;
    for (change_name, gherkin_text) in &docs {
        let _ = tx.send(ProcessingEvent::Status(format!(
            "📦 Exporting to OpenSpec: {}…", change_name
        )));

        match crate::openspec::generate(&openspec_url, change_name, gherkin_text, true).await {
            Ok(resp) => {
                // Save to disk if output directory is set
                let saved_paths = if let Some(ref dir) = openspec_output_dir {
                    match crate::openspec::save_artifacts(dir, &resp) {
                        Ok(paths) => {
                            let save_dir = dir.join("openspec").join(&resp.change_name);
                            let _ = tx.send(ProcessingEvent::Status(format!(
                                "💾 Saved {} OpenSpec artifacts to: {}",
                                paths.len(),
                                save_dir.display()
                            )));
                            paths
                        }
                        Err(e) => {
                            let _ = tx.send(ProcessingEvent::Status(format!(
                                "⚠ Failed to save OpenSpec artifacts for {}: {}", change_name, e
                            )));
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };

                let result = crate::openspec::OpenSpecExportResult {
                    change_name: resp.change_name.clone(),
                    feature_title: resp.feature_title,
                    artifacts: resp.artifacts,
                    saved_paths,
                };
                let _ = tx.send(ProcessingEvent::OpenSpecResult {
                    change_name: resp.change_name,
                    result,
                });
                ok_count += 1;
            }
            Err(e) => {
                let _ = tx.send(ProcessingEvent::Status(format!(
                    "⚠ OpenSpec export failed for {}: {}", change_name, e
                )));
            }
        }
    }

    let _ = tx.send(ProcessingEvent::OpenSpecDone(Ok(ok_count)));
}
