//! Project-level context that is accumulated across all parsed files.
//!
//! When multiple files are processed together, the `ProjectContext` keeps track
//! of every piece of information extracted so far.  This lets the LLM generate
//! Gherkin scenarios that correctly reference entities (actors, systems, data
//! fields, etc.) that may be defined in *other* files than the one currently
//! being transformed.

use std::collections::HashMap;
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
        if self.file_contents.is_empty() {
            return "No prior files have been processed yet.".to_string();
        }

        let mut summary = String::from("=== Cross-file project context ===\n\n");
        for (path, content) in &self.file_contents {
            summary.push_str(&format!("File: {}\nType: {}\n", path, content.file_type));
            // Include the first 400 chars of each file as a compact excerpt
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
