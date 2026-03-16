//! Session persistence — save and restore project state across app restarts.
//!
//! Stores a JSON file at `<output_dir>/.dockock_session.json` containing the
//! file list, groups, results, model selections, and user ratings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::context::FileGroup;
use crate::gherkin::GherkinDocument;

const SESSION_FILE: &str = ".dockock_session.json";

/// User quality rating for a generated result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rating {
    ThumbsUp,
    ThumbsDown,
}

/// Serializable snapshot of the application state.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionData {
    pub files: Vec<PathBuf>,
    pub groups: Vec<FileGroup>,
    pub results: HashMap<String, GherkinDocument>,
    pub group_results: HashMap<String, GherkinDocument>,
    pub ratings: HashMap<String, Rating>,
    pub generator_model: String,
    pub extractor_model: String,
    pub reviewer_model: String,
    pub vision_model: String,
    pub pipeline_mode: crate::llm::PipelineMode,
    pub max_concurrent: usize,
    pub output_dir: Option<PathBuf>,
    /// Previous results kept for diffing on regeneration.
    pub previous_results: HashMap<String, GherkinDocument>,
    pub previous_group_results: HashMap<String, GherkinDocument>,
}

/// Build the session file path from the output directory.
fn session_path(output_dir: &Path) -> PathBuf {
    output_dir.join(SESSION_FILE)
}

/// Save session data to disk. Returns `Ok(path)` on success.
#[instrument(skip(data))]
pub fn save(output_dir: &Path, data: &SessionData) -> Result<PathBuf, String> {
    let path = session_path(output_dir);
    let json = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write session: {}", e))?;
    Ok(path)
}

/// Load session data from disk. Returns `None` if the file doesn't exist.
#[instrument]
pub fn load(output_dir: &Path) -> Option<SessionData> {
    let path = session_path(output_dir);
    let json = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&json).ok()
}

/// Check whether a session file exists in the given output directory.
pub fn exists(output_dir: &Path) -> bool {
    session_path(output_dir).exists()
}

/// Compute a simple line-level diff between two Gherkin texts.
/// Returns a vec of `DiffLine` entries.
pub fn diff_gherkin(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple LCS-based diff using a DP approach (adequate for typical Gherkin sizes).
    let n = old_lines.len();
    let m = new_lines.len();

    // Build LCS table
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            if old_lines[i - 1] == new_lines[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    // Backtrack to produce diff
    let mut result = Vec::new();
    let mut i = n;
    let mut j = m;
    let mut stack = Vec::new();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_lines[i - 1] == new_lines[j - 1] {
            stack.push(DiffLine::Unchanged(old_lines[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            stack.push(DiffLine::Added(new_lines[j - 1].to_string()));
            j -= 1;
        } else {
            stack.push(DiffLine::Removed(old_lines[i - 1].to_string()));
            i -= 1;
        }
    }

    stack.reverse();
    result.extend(stack);
    result
}

/// A single line in a diff output.
#[derive(Debug, Clone)]
pub enum DiffLine {
    Unchanged(String),
    Added(String),
    Removed(String),
}
