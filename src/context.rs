//! Project-level context that is accumulated across all parsed files.
//!
//! When multiple files are processed together, the `ProjectContext` keeps track
//! of every piece of information extracted so far.  This lets the LLM generate
//! Gherkin scenarios that correctly reference entities (actors, systems, data
//! fields, etc.) that may be defined in *other* files than the one currently
//! being transformed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::parser::FileRole;

/// A single piece of extracted content from one file.
#[derive(Debug, Clone)]
pub struct FileContent {
    /// Original file path
    pub path: PathBuf,
    /// Human-readable file type label (e.g. "Word", "Excel", "Visio")
    pub file_type: String,
    /// Raw text / structured text extracted from the file
    pub raw_text: String,
    /// Whether this file produces Gherkin output or only provides context.
    pub role: FileRole,
}

/// A named collection of files whose content is merged into a single
/// aggregated context before Gherkin generation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Budget-aware: distributes `budget` chars proportionally across files so
    /// that ALL files are represented even when there are many.  Pass `0` to
    /// fall back to the unbounded (legacy) behaviour.
    pub fn build_summary(&self) -> String {
        self.build_summary_excluding(&HashSet::new(), 0)
    }

    /// Convenience wrapper with a character budget.
    pub fn build_summary_with_budget(&self, budget: usize) -> String {
        self.build_summary_excluding(&HashSet::new(), budget)
    }

    /// Build a compact summary, excluding files whose path string is in `exclude`.
    ///
    /// Used by group processing to avoid injecting the group's own members as
    /// cross-file context (they are already in the merged prompt).
    /// When `budget > 0`, each file receives a proportional share so all files
    /// are represented.  When `budget == 0`, the full raw text is included
    /// (legacy behaviour kept for cache-key hashing and backwards compat).
    pub fn build_summary_excluding(&self, exclude: &HashSet<String>, budget: usize) -> String {
        let included: Vec<(&String, &FileContent)> = self
            .file_contents
            .iter()
            .filter(|(path, _)| !exclude.contains(path.as_str()))
            .collect();

        if included.is_empty() {
            return "No prior files have been processed yet.".to_string();
        }

        let header = "=== Cross-file project context ===\n\n";

        // Entity block overhead
        let entity_block: String = if self.entities.is_empty() {
            String::new()
        } else {
            let mut eb = String::from("Known entities / actors / systems across all files:\n");
            for e in &self.entities {
                eb.push_str(&format!("  - {}\n", e));
            }
            eb
        };

        // Budget-aware path: give each file a proportional share
        if budget > 0 {
            let overhead = header.len() + entity_block.len();
            let available = budget.saturating_sub(overhead);
            // Per-file header: "File: <path>\nType: <type>\nContent:\n" ≈ 40 + path + type
            let per_file_header_est = 40_usize;
            let per_file = (available / included.len().max(1)).saturating_sub(per_file_header_est);

            let mut summary = String::from(header);
            for (path, content) in &included {
                summary.push_str(&format!("File: {}\nType: {}\n", path, content.file_type));
                let chars: usize = content.raw_text.chars().count();
                if per_file == 0 {
                    // At least mention the file exists
                    summary.push_str("[content omitted — budget exhausted]\n\n");
                } else if chars <= per_file {
                    summary.push_str(&format!("Content:\n{}\n\n", content.raw_text));
                } else {
                    let excerpt: String = content.raw_text.chars().take(per_file).collect();
                    summary.push_str(&format!("Content:\n{}\n[… truncated …]\n\n", excerpt));
                }
            }
            summary.push_str(&entity_block);
            return summary;
        }

        // Unbounded path (budget == 0): include full raw text
        let mut summary = String::from(header);
        for (path, content) in &included {
            summary.push_str(&format!("File: {}\nType: {}\n", path, content.file_type));
            summary.push_str(&format!("Content:\n{}\n\n", content.raw_text));
        }
        summary.push_str(&entity_block);
        summary
    }

    /// Build a combined context string from all context-only files (Excel, Visio).
    ///
    /// Injected into every primary-file prompt so the LLM sees reference data
    /// without attempting to generate Gherkin for it.
    /// When `budget > 0`, each file receives a proportional share.
    pub fn build_context_only_summary(&self) -> String {
        self.build_context_only_summary_with_budget(0)
    }

    /// Budget-aware version of `build_context_only_summary`.
    pub fn build_context_only_summary_with_budget(&self, budget: usize) -> String {
        let ctx_files: Vec<&FileContent> = self
            .file_contents
            .values()
            .filter(|fc| fc.role == FileRole::Context)
            .collect();
        if ctx_files.is_empty() {
            return String::new();
        }
        let header =
            "=== REFERENCE DATA (Excel / Visio — do NOT generate scenarios for these) ===\n\n";

        if budget > 0 {
            let available = budget.saturating_sub(header.len());
            let per_file_header_est = 40_usize;
            let per_file = (available / ctx_files.len().max(1)).saturating_sub(per_file_header_est);

            let mut summary = String::from(header);
            for fc in &ctx_files {
                let name = fc
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if per_file == 0 {
                    summary.push_str(&format!(
                        "--- Reference: {} ({}) ---\n[content omitted — budget exhausted]\n\n",
                        name, fc.file_type
                    ));
                } else if fc.raw_text.chars().count() <= per_file {
                    summary.push_str(&format!(
                        "--- Reference: {} ({}) ---\n{}\n\n",
                        name, fc.file_type, fc.raw_text
                    ));
                } else {
                    let excerpt: String = fc.raw_text.chars().take(per_file).collect();
                    summary.push_str(&format!(
                        "--- Reference: {} ({}) ---\n{}\n[… truncated …]\n\n",
                        name, fc.file_type, excerpt
                    ));
                }
            }
            return summary;
        }

        // Unbounded path: full raw text
        let mut summary = String::from(header);
        for fc in &ctx_files {
            let name = fc
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            summary.push_str(&format!(
                "--- Reference: {} ({}) ---\n{}\n\n",
                name, fc.file_type, fc.raw_text
            ));
        }
        summary
    }

    /// Clear all accumulated context (used when starting a fresh processing run).
    pub fn clear(&mut self) {
        self.file_contents.clear();
        self.entities.clear();
        self.notes.clear();
    }

    /// Extract named entities (actors, systems, data objects) from all file contents.
    ///
    /// Uses heuristic patterns to find capitalised multi-word terms and known
    /// keywords that indicate actors, systems, or data objects. The results
    /// populate `self.entities` for injection into LLM prompts as a glossary.
    pub fn extract_entities(&mut self) {
        let mut found: HashSet<String> = HashSet::new();

        // Keywords that often precede actor/system/entity names
        let actor_signals = [
            "the ", "a ", "an ", "user ", "admin ", "manager ", "system ",
            "service ", "module ", "component ", "server ", "client ",
            "portal ", "gateway ", "engine ", "agent ", "api ",
        ];

        for content in self.file_contents.values() {
            let text = &content.raw_text;

            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Pattern 1: Capitalised multi-word terms (e.g. "Invoice Processing System")
                // Look for runs of 2-5 capitalised words
                let words: Vec<&str> = trimmed.split_whitespace().collect();
                let mut i = 0;
                while i < words.len() {
                    if starts_with_upper(words[i]) && words[i].len() > 1 && words[i].chars().all(|c| c.is_alphanumeric()) {
                        let mut j = i + 1;
                        while j < words.len()
                            && j - i < 5
                            && starts_with_upper(words[j])
                            && words[j].len() > 1
                            && words[j].chars().all(|c| c.is_alphanumeric())
                        {
                            j += 1;
                        }
                        if j - i >= 2 {
                            let term: String = words[i..j].join(" ");
                            // Filter out common non-entity phrases
                            if !is_noise_phrase(&term) {
                                found.insert(term);
                            }
                        }
                        i = j;
                    } else {
                        i += 1;
                    }
                }

                // Pattern 2: Terms after actor signal keywords in table cells / labels
                let lower = trimmed.to_lowercase();
                for signal in &actor_signals {
                    if let Some(pos) = lower.find(signal) {
                        let after = &trimmed[pos + signal.len()..];
                        let candidate: String = after
                            .split(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if candidate.len() >= 3 && starts_with_upper(&candidate) && !is_noise_phrase(&candidate) {
                            found.insert(candidate);
                        }
                    }
                }
            }
        }

        // Sort and deduplicate
        let mut entities: Vec<String> = found.into_iter().collect();
        entities.sort();
        self.entities = entities;
    }

    /// Chunk all file contents into overlapping text chunks for RAG indexing.
    pub fn chunk_all_files(&self) -> Vec<crate::rag::TextChunk> {
        self.file_contents
            .values()
            .flat_map(|fc| {
                let name = fc.path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| fc.path.to_string_lossy().to_string());
                crate::rag::chunk_text(&fc.raw_text, &name, &fc.file_type)
            })
            .collect()
    }

    /// Build a glossary string for injection into LLM prompts.
    ///
    /// Returns an empty string if no entities have been extracted.
    pub fn build_glossary(&self) -> String {
        if self.entities.is_empty() {
            return String::new();
        }

        let mut glossary = String::from("=== PROJECT GLOSSARY ===\n");
        glossary.push_str("The following named entities were found across all project documents.\n");
        glossary.push_str("Use ONLY these terms (or close variants) in your Gherkin scenarios:\n\n");
        for entity in &self.entities {
            glossary.push_str(&format!("  - {}\n", entity));
        }
        glossary.push('\n');
        glossary
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

/// Check if a string starts with an uppercase letter.
fn starts_with_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

/// Filter out common capitalised phrases that are not meaningful entities.
fn is_noise_phrase(s: &str) -> bool {
    const NOISE: &[&str] = &[
        "The", "This", "That", "These", "Those", "There",
        "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday",
        "January", "February", "March", "April", "May", "June",
        "July", "August", "September", "October", "November", "December",
        "True", "False", "Yes", "No", "None", "All", "Any",
        "Table", "Figure", "Page", "Section", "Chapter", "Appendix",
        "Note", "Version", "Document", "Sheet", "Row", "Column",
    ];
    if NOISE.contains(&s) {
        return true;
    }
    // Very short or very long phrases are unlikely to be useful entities
    s.len() < 4 || s.len() > 60
}
