//! Project-level context that is accumulated across all parsed files.
//!
//! When multiple files are processed together, the `ProjectContext` keeps track
//! of every piece of information extracted so far.  This lets the LLM generate
//! Gherkin scenarios that correctly reference entities (actors, systems, data
//! fields, etc.) that may be defined in *other* files than the one currently
//! being transformed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// A single piece of extracted content from one file.
#[derive(Debug, Clone)]
pub struct FileContent {
    /// Original file path
    pub path: PathBuf,
    /// Human-readable file type label (e.g. "Word", "Excel", "Visio")
    pub file_type: String,
    /// Raw text / structured text extracted from the file
    pub raw_text: String,
}

/// A named collection of files whose content is merged into a single
/// aggregated context before Gherkin generation.
#[derive(Debug, Clone)]
pub struct FileGroup {
    /// Human-readable group label (e.g. "D028_Requirements")
    pub name: String,
    /// Ordered list of file paths that belong to this group
    pub members: Vec<PathBuf>,
    /// `true` when the group was manually created by the user (not auto-detected).
    /// Manual groups are kept even when empty so the user can add files later.
    pub manual: bool,
}

/// Accumulated project context shared across all files being processed.
#[derive(Debug, Default, Clone)]
pub struct ProjectContext {
    /// Extracted content from each processed file, keyed by absolute path string
    pub file_contents: HashMap<String, FileContent>,
    /// Named entities discovered across all files (actors, systems, data fields)
    pub entities: Vec<String>,
    /// Free-form notes appended as new files are processed
    pub notes: Vec<String>,
}

impl ProjectContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the extracted content of a file into the context.
    pub fn add_file(&mut self, content: FileContent) {
        self.file_contents
            .insert(content.path.to_string_lossy().to_string(), content);
    }

    /// Build a compact summary string suitable for injecting into the LLM prompt.
    ///
    /// The summary lists every file that has already been processed along with a
    /// short excerpt of its raw text so that the model has cross-file awareness.
    pub fn build_summary(&self) -> String {
        self.build_summary_excluding(&HashSet::new())
    }

    /// Build a compact summary, excluding files whose path string is in `exclude`.
    ///
    /// Used by group processing to avoid injecting the group's own members as
    /// cross-file context (they are already in the merged prompt).
    pub fn build_summary_excluding(&self, exclude: &HashSet<String>) -> String {
        let included: HashMap<&String, &FileContent> = self
            .file_contents
            .iter()
            .filter(|(path, _)| !exclude.contains(path.as_str()))
            .collect();

        if included.is_empty() {
            return "No prior files have been processed yet.".to_string();
        }

        let mut summary = String::from("=== Cross-file project context ===\n\n");
        for (path, content) in &included {
            summary.push_str(&format!("File: {}\nType: {}\n", path, content.file_type));
            let excerpt: String = content.raw_text.chars().take(400).collect();
            summary.push_str(&format!("Excerpt:\n{}\n\n", excerpt));
        }

        if !self.entities.is_empty() {
            summary.push_str("Known entities / actors / systems across all files:\n");
            for e in &self.entities {
                summary.push_str(&format!("  - {}\n", e));
            }
        }

        summary
    }

    /// Clear all accumulated context (used when starting a fresh processing run).
    pub fn clear(&mut self) {
        self.file_contents.clear();
        self.entities.clear();
        self.notes.clear();
    }
}

/// Compute automatic file groups from a list of paths.
///
/// Files whose file stems (filename without extension) are identical are placed
/// into the same group.  E.g. `D028_Req.docx` + `D028_Req.vsdx` → group "D028_Req".
/// Files with unique stems are left ungrouped (not returned).
pub fn compute_auto_groups(files: &[PathBuf]) -> Vec<FileGroup> {
    let mut stem_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in files {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !stem.is_empty() {
            stem_map.entry(stem).or_default().push(path.clone());
        }
    }

    let mut groups: Vec<FileGroup> = stem_map
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(name, members)| FileGroup { name, members, manual: false })
        .collect();

    groups.sort_by(|a, b| a.name.cmp(&b.name));
    groups
}
