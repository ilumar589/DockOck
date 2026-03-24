//! Validation source-of-truth — compare generated output against approved golden files.
//!
//! Users import `.feature` and `.md` files as a "golden set". The pipeline uses
//! these to extract systematic patterns of difference, then applies corrections
//! to all generated artifacts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gherkin::{GherkinDocument, StepKeyword};
use crate::markdown::MarkdownDocument;

// ─────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────

/// File extensions accepted for validation files.
pub const VALIDATION_EXTENSIONS: &[&str] = &["feature", "md"];

// ─────────────────────────────────────────────
// Data model
// ─────────────────────────────────────────────

/// The format of a validation file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationKind {
    /// A `.feature` Gherkin file.
    Gherkin,
    /// A `.md` Markdown file.
    Markdown,
}

/// A parsed validation file ready for comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationFile {
    /// Original file path on disk.
    pub path: PathBuf,
    /// Stem used for matching (e.g. `"D028_Req"`).
    pub match_key: String,
    /// The format of this validation file.
    pub kind: ValidationKind,
    /// Parsed Gherkin document (if `.feature`).
    pub gherkin: Option<GherkinDocument>,
    /// Parsed Markdown document (if `.md`).
    pub markdown: Option<MarkdownDocument>,
    /// Raw text content (for fallback diffing).
    pub raw_text: String,
}

/// Normalised Gherkin representation for structural comparison.
#[derive(Debug, Clone)]
pub struct NormalisedGherkin {
    pub feature_title: String,
    pub scenarios: Vec<NormalisedScenario>,
}

/// A normalised scenario with lowercased, trimmed steps sorted for comparison.
#[derive(Debug, Clone)]
pub struct NormalisedScenario {
    pub title: String,
    pub title_lower: String,
    pub steps: Vec<NormalisedStep>,
    pub is_outline: bool,
}

/// A normalised step for structural comparison.
#[derive(Debug, Clone)]
pub struct NormalisedStep {
    pub keyword: StepKeyword,
    pub text_lower: String,
}

/// Diff between a generated/golden pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairDiff {
    pub source: String,
    pub missing_scenarios: Vec<String>,
    pub extra_scenarios: Vec<String>,
    pub scenario_diffs: Vec<ScenarioDiff>,
    pub style_diffs: Vec<StyleDiff>,
}

/// Step-level diff for a matched scenario pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioDiff {
    pub title: String,
    pub missing_steps: Vec<String>,
    pub extra_steps: Vec<String>,
    pub modified_steps: Vec<(String, String)>,
}

/// Style/convention difference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StyleDiff {
    TagConvention { expected: String, actual: String },
    NamingPattern { expected_pattern: String, example: String },
    StepPhrasing { category: String, expected: String, actual: String },
    StructuralPattern { description: String },
}

/// A pattern extracted from cross-pair analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffPattern {
    InventedContent { category: String, examples: Vec<String> },
    MissingContent { category: String, examples: Vec<String> },
    TerminologyMismatch { generated_term: String, golden_term: String },
    LifecycleMisplacement { concept: String, generated_phase: String, golden_phase: String },
    OptionalityMismatch { field: String, generated: String, golden: String },
    CardinalityMismatch { field: String, generated: String, golden: String },
    KeywordUsage { description: String },
}

/// Aggregated patterns across all diff pairs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregatedPatterns {
    /// Patterns with their frequency count.
    pub recurring: Vec<(DiffPattern, usize)>,
    /// Style conventions observed in golden files but not generated.
    pub conventions: Vec<String>,
    /// Common structural rules (e.g. "always include a Background block").
    pub structural_rules: Vec<String>,
    /// Raw LLM-extracted pattern text (human-readable summary).
    pub llm_pattern_summary: Option<String>,
}

/// Markdown pair diff (analogous to PairDiff for Gherkin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownPairDiff {
    pub source: String,
    pub missing_sections: Vec<String>,
    pub extra_sections: Vec<String>,
    pub style_diffs: Vec<StyleDiff>,
}

// ─────────────────────────────────────────────
// Parsing & import
// ─────────────────────────────────────────────

impl ValidationFile {
    /// Parse a validation file from disk.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        let raw_text = std::fs::read_to_string(path).ok()?;
        let match_key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        match ext.as_str() {
            "feature" => {
                let gherkin = GherkinDocument::parse_from_llm_output(&raw_text, file_name);
                Some(ValidationFile {
                    path: path.to_path_buf(),
                    match_key,
                    kind: ValidationKind::Gherkin,
                    gherkin: Some(gherkin),
                    markdown: None,
                    raw_text,
                })
            }
            "md" => {
                let markdown = MarkdownDocument::parse_from_llm_output(&raw_text, file_name);
                Some(ValidationFile {
                    path: path.to_path_buf(),
                    match_key,
                    kind: ValidationKind::Markdown,
                    gherkin: None,
                    markdown: Some(markdown),
                    raw_text,
                })
            }
            _ => None,
        }
    }

    /// Brief description for UI display.
    pub fn summary(&self) -> String {
        match self.kind {
            ValidationKind::Gherkin => {
                let count = self
                    .gherkin
                    .as_ref()
                    .map(|g| g.scenarios.len())
                    .unwrap_or(0);
                format!("{} scenario(s)", count)
            }
            ValidationKind::Markdown => {
                let count = self
                    .markdown
                    .as_ref()
                    .map(|m| m.sections.len())
                    .unwrap_or(0);
                format!("{} section(s)", count)
            }
        }
    }
}

/// Recursively collect validation files (`.feature`, `.md`) from a directory.
pub fn collect_validation_files(root: &Path) -> Vec<PathBuf> {
    let accepted: std::collections::HashSet<&str> =
        VALIDATION_EXTENSIONS.iter().copied().collect();
    let mut results = Vec::new();

    fn walk(
        dir: &Path,
        accepted: &std::collections::HashSet<&str>,
        out: &mut Vec<PathBuf>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if name.starts_with('.') || name.starts_with('~') {
                continue;
            }
            if path.is_dir() {
                walk(&path, accepted, out);
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if accepted.contains(ext.to_lowercase().as_str()) {
                    out.push(path);
                }
            }
        }
    }

    walk(root, &accepted, &mut results);
    results.sort();
    results
}

// ─────────────────────────────────────────────
// Normalisation
// ─────────────────────────────────────────────

impl GherkinDocument {
    /// Returns a normalised representation suitable for structural comparison.
    pub fn to_normalised(&self) -> NormalisedGherkin {
        let mut scenarios: Vec<NormalisedScenario> = self
            .scenarios
            .iter()
            .map(|s| NormalisedScenario {
                title: s.title.clone(),
                title_lower: s.title.trim().to_lowercase(),
                steps: s
                    .steps
                    .iter()
                    .map(|step| NormalisedStep {
                        keyword: step.keyword.clone(),
                        text_lower: step.text.trim().to_lowercase(),
                    })
                    .collect(),
                is_outline: s.is_outline,
            })
            .collect();
        scenarios.sort_by(|a, b| a.title_lower.cmp(&b.title_lower));
        NormalisedGherkin {
            feature_title: self.feature_title.trim().to_lowercase(),
            scenarios,
        }
    }
}

// ─────────────────────────────────────────────
// Matching: generated ↔ validation
// ─────────────────────────────────────────────

/// Match result pairing a generated artifact key with its golden validation file.
pub struct MatchedPair {
    pub generated_key: String,
    pub validation_file: ValidationFile,
}

/// Match generated artifacts against validation files.
///
/// `generated_docs` is an optional parallel slice of parsed GherkinDocuments
/// (same order/length as `generated_keys`) used for feature-title matching.
///
/// Returns matched pairs and a list of unmatched validation file keys.
pub fn match_validation_files(
    generated_keys: &[String],
    generated_docs: Option<&[GherkinDocument]>,
    validation_files: &[ValidationFile],
) -> (Vec<MatchedPair>, Vec<String>) {
    let mut matched = Vec::new();
    let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (gen_idx, gen_key) in generated_keys.iter().enumerate() {
        let gen_stem = Path::new(gen_key)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(gen_key)
            .to_lowercase();
        let gen_desc = extract_descriptive_part(&gen_stem);

        // 1. Exact stem match
        if let Some((i, vf)) = validation_files.iter().enumerate().find(|(i, vf)| {
            !used.contains(i) && vf.match_key.to_lowercase() == gen_stem
        }) {
            matched.push(MatchedPair {
                generated_key: gen_key.clone(),
                validation_file: vf.clone(),
            });
            used.insert(i);
            continue;
        }

        // 2. Prefix/suffix match on full stems
        if let Some((i, vf)) = validation_files.iter().enumerate().find(|(i, vf)| {
            if used.contains(i) {
                return false;
            }
            let vk = vf.match_key.to_lowercase();
            gen_stem.starts_with(&vk) || vk.starts_with(&gen_stem)
        }) {
            matched.push(MatchedPair {
                generated_key: gen_key.clone(),
                validation_file: vf.clone(),
            });
            used.insert(i);
            continue;
        }

        // 3. Descriptive-part match — strip document-code prefixes and compare
        //    e.g. "S747 - LNA - Create premises" → "create premises"
        //         "6.2.1.1 Create premises"       → "create premises"
        if !gen_desc.is_empty() {
            // 3a. Exact descriptive-part match
            if let Some((i, vf)) = validation_files.iter().enumerate().find(|(i, vf)| {
                if used.contains(i) { return false; }
                let vf_desc = extract_descriptive_part(&vf.match_key.to_lowercase());
                !vf_desc.is_empty() && vf_desc == gen_desc
            }) {
                matched.push(MatchedPair {
                    generated_key: gen_key.clone(),
                    validation_file: vf.clone(),
                });
                used.insert(i);
                continue;
            }

            // 3b. Substring containment on descriptive parts
            if let Some((i, vf)) = validation_files.iter().enumerate().find(|(i, vf)| {
                if used.contains(i) { return false; }
                let vf_desc = extract_descriptive_part(&vf.match_key.to_lowercase());
                !vf_desc.is_empty() && vf_desc.len() >= 5
                    && (vf_desc.contains(&gen_desc) || gen_desc.contains(&vf_desc))
            }) {
                matched.push(MatchedPair {
                    generated_key: gen_key.clone(),
                    validation_file: vf.clone(),
                });
                used.insert(i);
                continue;
            }
        }

        // 4. Feature title matching — compare generated feature_title to validation feature_title
        if let Some(gen_doc) = generated_docs.and_then(|docs| docs.get(gen_idx)) {
            let gen_title = gen_doc.feature_title.trim().to_lowercase();
            if !gen_title.is_empty() {
                // 4a. Exact feature title match
                if let Some((i, vf)) = validation_files.iter().enumerate().find(|(i, vf)| {
                    if used.contains(i) { return false; }
                    if let Some(ref g) = vf.gherkin {
                        g.feature_title.trim().to_lowercase() == gen_title
                    } else {
                        false
                    }
                }) {
                    matched.push(MatchedPair {
                        generated_key: gen_key.clone(),
                        validation_file: vf.clone(),
                    });
                    used.insert(i);
                    continue;
                }

                // 4b. Fuzzy feature title match (threshold 0.7)
                let mut best: Option<(usize, f64)> = None;
                for (i, vf) in validation_files.iter().enumerate() {
                    if used.contains(&i) { continue; }
                    if let Some(ref g) = vf.gherkin {
                        let vf_title = g.feature_title.trim().to_lowercase();
                        if !vf_title.is_empty() {
                            let ratio = levenshtein_ratio(&gen_title, &vf_title);
                            if ratio > 0.7 {
                                if best.map_or(true, |(_, br)| ratio > br) {
                                    best = Some((i, ratio));
                                }
                            }
                        }
                    }
                }
                if let Some((i, _)) = best {
                    matched.push(MatchedPair {
                        generated_key: gen_key.clone(),
                        validation_file: validation_files[i].clone(),
                    });
                    used.insert(i);
                    continue;
                }
            }
        }

        // 5. Fuzzy descriptive-part match (threshold 0.75)
        if !gen_desc.is_empty() {
            let mut best: Option<(usize, f64)> = None;
            for (i, vf) in validation_files.iter().enumerate() {
                if used.contains(&i) { continue; }
                let vf_desc = extract_descriptive_part(&vf.match_key.to_lowercase());
                if vf_desc.len() >= 4 {
                    let ratio = levenshtein_ratio(&gen_desc, &vf_desc);
                    if ratio > 0.75 {
                        if best.map_or(true, |(_, br)| ratio > br) {
                            best = Some((i, ratio));
                        }
                    }
                }
            }
            if let Some((i, _)) = best {
                matched.push(MatchedPair {
                    generated_key: gen_key.clone(),
                    validation_file: validation_files[i].clone(),
                });
                used.insert(i);
            }
        }
    }

    let unmatched: Vec<String> = validation_files
        .iter()
        .enumerate()
        .filter(|(i, _)| !used.contains(i))
        .map(|(_, vf)| vf.match_key.clone())
        .collect();

    (matched, unmatched)
}

/// Extract the descriptive part of a file stem by stripping common prefixes.
///
/// Handles patterns like:
/// - `s747 - lna - create premises` → `create premises`
/// - `d028 - ius - xml disconnection notification` → `xml disconnection notification`
/// - `6.2.1.1 create premises` → `create premises`
/// - `6.2.1.1 create premises unhappy` → `create premises unhappy`
/// - `create premises` → `create premises` (no prefix)
fn extract_descriptive_part(stem: &str) -> String {
    let s = stem.trim();

    // Pattern 1: "S747 - LNA - Description" or "D028 – IUS – Description"
    // Match: code - system - description (with - or –)
    if let Some(rest) = strip_multi_dash_prefix(s) {
        return rest.trim().to_string();
    }

    // Pattern 2: "6.2.1.1 Description" — dotted numeric prefix followed by space
    let trimmed = s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if trimmed.len() < s.len() && trimmed.starts_with(|c: char| c == ' ') {
        return trimmed.trim().to_string();
    }

    // No prefix found — return as-is
    s.to_string()
}

/// Strip a multi-segment dash prefix like "S747 - LNA - " or "D028 – IUS – ".
/// Returns the remainder after the second dash separator.
fn strip_multi_dash_prefix(s: &str) -> Option<&str> {
    // Find the first dash separator (either " - " or " – ")
    let first_sep = find_dash_separator(s)?;
    let after_first = &s[first_sep..].trim_start_matches(|c: char| c == '-' || c == '–' || c == ' ');

    // Find the second dash separator
    if let Some(second_sep) = find_dash_separator(after_first) {
        let desc = &after_first[second_sep..].trim_start_matches(|c: char| c == '-' || c == '–' || c == ' ');
        if !desc.is_empty() {
            return Some(desc);
        }
    }
    None
}

/// Find the byte offset of the first " - " or " – " separator.
fn find_dash_separator(s: &str) -> Option<usize> {
    // " - " (space-hyphen-space)
    if let Some(pos) = s.find(" - ") {
        return Some(pos + 3);
    }
    // " – " (space-en-dash-space)
    if let Some(pos) = s.find(" – ") {
        return Some(pos + " – ".len());
    }
    // " — " (space-em-dash-space)
    if let Some(pos) = s.find(" — ") {
        return Some(pos + " — ".len());
    }
    None
}

/// Simple Levenshtein distance ratio (0.0–1.0, where 1.0 = identical).
fn levenshtein_ratio(a: &str, b: &str) -> f64 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    if a_len == 0 && b_len == 0 {
        return 1.0;
    }
    let max_len = a_len.max(b_len);

    let mut prev = (0..=b_len).collect::<Vec<_>>();
    let mut curr = vec![0; b_len + 1];

    for i in 1..=a_len {
        curr[0] = i;
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    1.0 - (prev[b_len] as f64 / max_len as f64)
}

// ─────────────────────────────────────────────
// Structural diff
// ─────────────────────────────────────────────

/// Compute a structural diff between a generated and golden Gherkin document.
pub fn diff_gherkin_pair(
    generated: &GherkinDocument,
    golden: &GherkinDocument,
    source: &str,
) -> PairDiff {
    let gen_norm = generated.to_normalised();
    let gold_norm = golden.to_normalised();

    let gen_titles: std::collections::HashSet<String> =
        gen_norm.scenarios.iter().map(|s| s.title_lower.clone()).collect();
    let gold_titles: std::collections::HashSet<String> =
        gold_norm.scenarios.iter().map(|s| s.title_lower.clone()).collect();

    let missing_scenarios: Vec<String> = gold_norm
        .scenarios
        .iter()
        .filter(|s| !gen_titles.contains(&s.title_lower))
        .map(|s| s.title.clone())
        .collect();

    let extra_scenarios: Vec<String> = gen_norm
        .scenarios
        .iter()
        .filter(|s| !gold_titles.contains(&s.title_lower))
        .map(|s| s.title.clone())
        .collect();

    // Diff matched scenarios at the step level
    let mut scenario_diffs = Vec::new();
    for gold_sc in &gold_norm.scenarios {
        if let Some(gen_sc) = gen_norm
            .scenarios
            .iter()
            .find(|s| s.title_lower == gold_sc.title_lower)
        {
            let gen_steps: std::collections::HashSet<String> =
                gen_sc.steps.iter().map(|s| s.text_lower.clone()).collect();
            let gold_steps: std::collections::HashSet<String> =
                gold_sc.steps.iter().map(|s| s.text_lower.clone()).collect();

            let missing_steps: Vec<String> = gold_sc
                .steps
                .iter()
                .filter(|s| !gen_steps.contains(&s.text_lower))
                .map(|s| format!("{} {}", s.keyword.as_str(), s.text_lower.clone()))
                .collect();

            let extra_steps: Vec<String> = gen_sc
                .steps
                .iter()
                .filter(|s| !gold_steps.contains(&s.text_lower))
                .map(|s| format!("{} {}", s.keyword.as_str(), s.text_lower.clone()))
                .collect();

            if !missing_steps.is_empty() || !extra_steps.is_empty() {
                scenario_diffs.push(ScenarioDiff {
                    title: gold_sc.title.clone(),
                    missing_steps,
                    extra_steps,
                    modified_steps: Vec::new(),
                });
            }
        }
    }

    PairDiff {
        source: source.to_string(),
        missing_scenarios,
        extra_scenarios,
        scenario_diffs,
        style_diffs: Vec::new(),
    }
}

/// Compute a structural diff between generated and golden Markdown documents.
pub fn diff_markdown_pair(
    generated: &MarkdownDocument,
    golden: &MarkdownDocument,
    source: &str,
) -> MarkdownPairDiff {
    let gen_headings: std::collections::HashSet<String> = generated
        .sections
        .iter()
        .map(|s| s.heading.trim().to_lowercase())
        .collect();
    let gold_headings: std::collections::HashSet<String> = golden
        .sections
        .iter()
        .map(|s| s.heading.trim().to_lowercase())
        .collect();

    let missing_sections: Vec<String> = golden
        .sections
        .iter()
        .filter(|s| !gen_headings.contains(&s.heading.trim().to_lowercase()))
        .map(|s| s.heading.clone())
        .collect();

    let extra_sections: Vec<String> = generated
        .sections
        .iter()
        .filter(|s| !gold_headings.contains(&s.heading.trim().to_lowercase()))
        .map(|s| s.heading.clone())
        .collect();

    MarkdownPairDiff {
        source: source.to_string(),
        missing_sections,
        extra_sections,
        style_diffs: Vec::new(),
    }
}

/// Aggregate diffs across multiple pairs into recurring patterns.
pub fn aggregate_patterns(diffs: &[PairDiff]) -> AggregatedPatterns {
    let mut patterns = AggregatedPatterns::default();

    // Collect invented content (extra scenarios across pairs)
    let mut extra_categories: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut missing_categories: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for diff in diffs {
        for sc in &diff.extra_scenarios {
            extra_categories
                .entry("extra_scenarios".to_string())
                .or_default()
                .push(format!("{}: {}", diff.source, sc));
        }
        for sc in &diff.missing_scenarios {
            missing_categories
                .entry("missing_scenarios".to_string())
                .or_default()
                .push(format!("{}: {}", diff.source, sc));
        }
        for sd in &diff.scenario_diffs {
            for step in &sd.extra_steps {
                extra_categories
                    .entry("extra_steps".to_string())
                    .or_default()
                    .push(format!("{}/{}: {}", diff.source, sd.title, step));
            }
            for step in &sd.missing_steps {
                missing_categories
                    .entry("missing_steps".to_string())
                    .or_default()
                    .push(format!("{}/{}: {}", diff.source, sd.title, step));
            }
        }
    }

    for (category, examples) in extra_categories {
        if examples.len() >= 2 {
            patterns.recurring.push((
                DiffPattern::InventedContent {
                    category: category.clone(),
                    examples: examples.clone(),
                },
                examples.len(),
            ));
        }
    }

    for (category, examples) in missing_categories {
        if examples.len() >= 2 {
            patterns.recurring.push((
                DiffPattern::MissingContent {
                    category: category.clone(),
                    examples: examples.clone(),
                },
                examples.len(),
            ));
        }
    }

    // Sort by frequency descending
    patterns.recurring.sort_by(|a, b| b.1.cmp(&a.1));

    patterns
}

/// Build the patterns block text for the correction LLM prompt.
pub fn build_patterns_block(patterns: &AggregatedPatterns) -> String {
    let mut out = String::new();

    if let Some(ref llm_summary) = patterns.llm_pattern_summary {
        out.push_str(llm_summary);
        out.push_str("\n\n");
    }

    if !patterns.recurring.is_empty() {
        out.push_str("RECURRING PATTERNS:\n");
        for (i, (pattern, count)) in patterns.recurring.iter().enumerate() {
            let desc = match pattern {
                DiffPattern::InventedContent { category, examples } => {
                    format!(
                        "Invented content ({}): generator adds content not in source. Examples: {}",
                        category,
                        examples.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
                    )
                }
                DiffPattern::MissingContent { category, examples } => {
                    format!(
                        "Missing content ({}): generator omits required content. Examples: {}",
                        category,
                        examples.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
                    )
                }
                DiffPattern::TerminologyMismatch {
                    generated_term,
                    golden_term,
                } => {
                    format!(
                        "Terminology: generator uses \"{}\" but should use \"{}\"",
                        generated_term, golden_term
                    )
                }
                DiffPattern::LifecycleMisplacement {
                    concept,
                    generated_phase,
                    golden_phase,
                } => {
                    format!(
                        "Lifecycle: \"{}\" placed in [{}] but should be in [{}]",
                        concept, generated_phase, golden_phase
                    )
                }
                DiffPattern::OptionalityMismatch {
                    field,
                    generated,
                    golden,
                } => {
                    format!(
                        "Optionality: field \"{}\" treated as {} but should be {}",
                        field, generated, golden
                    )
                }
                DiffPattern::CardinalityMismatch {
                    field,
                    generated,
                    golden,
                } => {
                    format!(
                        "Cardinality: field \"{}\" is {} but should be {}",
                        field, generated, golden
                    )
                }
                DiffPattern::KeywordUsage { description } => {
                    format!("Keyword usage: {}", description)
                }
            };
            out.push_str(&format!("{}. [{}x] {}\n", i + 1, count, desc));
        }
        out.push('\n');
    }

    if !patterns.conventions.is_empty() {
        out.push_str("CONVENTIONS:\n");
        for conv in &patterns.conventions {
            out.push_str(&format!("- {}\n", conv));
        }
        out.push('\n');
    }

    if !patterns.structural_rules.is_empty() {
        out.push_str("STRUCTURAL RULES:\n");
        for rule in &patterns.structural_rules {
            out.push_str(&format!("- {}\n", rule));
        }
        out.push('\n');
    }

    out
}

/// Compute an alignment score (0.0–1.0) between two texts based on edit distance.
pub fn alignment_score(generated: &str, golden: &str) -> f64 {
    levenshtein_ratio(
        &generated.trim().to_lowercase(),
        &golden.trim().to_lowercase(),
    )
}

/// Build the LLM prompt for pattern extraction from pairs.
pub fn build_pattern_extraction_prompt(
    pairs: &[(String, String)], // (generated_text, golden_text)
) -> String {
    let mut prompt = String::from(
        "You are a Gherkin quality analyst. Below are pairs of (GENERATED, GOLDEN) Gherkin\n\
         features for the same source document. The GOLDEN version is the approved\n\
         source of truth.\n\n",
    );

    for (i, (generated, golden)) in pairs.iter().enumerate() {
        prompt.push_str(&format!("=== PAIR {} ===\n", i + 1));
        prompt.push_str("GENERATED:\n");
        prompt.push_str(generated);
        prompt.push_str("\n\nGOLDEN:\n");
        prompt.push_str(golden);
        prompt.push_str("\n\n");
    }

    prompt.push_str(
        "Analyse ALL pairs and extract RECURRING PATTERNS of difference.\n\
         For each pattern, provide:\n\
         1. Category (Invented, Missing, Terminology, Lifecycle, Optionality, Cardinality, Structure, Style)\n\
         2. Description of what the generator consistently does wrong\n\
         3. Concrete correction rule (what should be done instead)\n\
         4. Confidence: HIGH (3+ examples) / MEDIUM (2 examples) / LOW (1 example)\n\n\
         Output as a numbered list of patterns. Focus on RECURRING issues, not one-off typos.",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_ratio_identical() {
        assert!((levenshtein_ratio("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_levenshtein_ratio_empty() {
        assert!((levenshtein_ratio("", "") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_levenshtein_ratio_different() {
        let ratio = levenshtein_ratio("kitten", "sitting");
        assert!(ratio > 0.5);
        assert!(ratio < 1.0);
    }

    #[test]
    fn test_match_exact_stem() {
        let vf = ValidationFile {
            path: PathBuf::from("D028_Req.feature"),
            match_key: "D028_Req".to_string(),
            kind: ValidationKind::Gherkin,
            gherkin: None,
            markdown: None,
            raw_text: String::new(),
        };
        let (matched, _) = match_validation_files(&["D028_Req.docx".to_string()], None, &[vf]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].generated_key, "D028_Req.docx");
    }

    #[test]
    fn test_match_prefix() {
        let vf = ValidationFile {
            path: PathBuf::from("D028_Req_v2.feature"),
            match_key: "D028_Req_v2".to_string(),
            kind: ValidationKind::Gherkin,
            gherkin: None,
            markdown: None,
            raw_text: String::new(),
        };
        let (matched, _) = match_validation_files(&["D028_Req.docx".to_string()], None, &[vf]);
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn test_alignment_score() {
        let score = alignment_score("Feature: Foo", "Feature: Foo");
        assert!((score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_extract_descriptive_part_story_code() {
        assert_eq!(
            extract_descriptive_part("s747 - lna - create premises"),
            "create premises"
        );
        assert_eq!(
            extract_descriptive_part("d028 – ius – xml disconnection notification"),
            "xml disconnection notification"
        );
    }

    #[test]
    fn test_extract_descriptive_part_dotted_prefix() {
        assert_eq!(
            extract_descriptive_part("6.2.1.1 create premises"),
            "create premises"
        );
        assert_eq!(
            extract_descriptive_part("6.2.3.3 change meter status"),
            "change meter status"
        );
    }

    #[test]
    fn test_extract_descriptive_part_no_prefix() {
        assert_eq!(
            extract_descriptive_part("create premises"),
            "create premises"
        );
    }

    #[test]
    fn test_match_cross_naming_scheme() {
        // Simulates: generated "S747 - LNA - Create premises" vs validation "6.2.1.1 Create premises"
        let vf = ValidationFile {
            path: PathBuf::from("6.2.1.1 Create premises.feature"),
            match_key: "6.2.1.1 Create premises".to_string(),
            kind: ValidationKind::Gherkin,
            gherkin: None,
            markdown: None,
            raw_text: String::new(),
        };
        let (matched, unmatched) = match_validation_files(
            &["S747 - LNA - Create premises".to_string()],
            None,
            &[vf],
        );
        assert_eq!(matched.len(), 1, "Should match via descriptive-part: matched={}, unmatched={:?}", matched.len(), unmatched);
        assert_eq!(matched[0].generated_key, "S747 - LNA - Create premises");
    }
}
