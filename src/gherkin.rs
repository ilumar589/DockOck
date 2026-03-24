//! Gherkin data-structures and formatting helpers.
//!
//! The `GherkinDocument` produced here follows the standard Gherkin grammar:
//!
//! ```text
//! Feature: <title>
//!   [Background: ...]
//!
//!   Scenario: <title>
//!     Given ...
//!     When  ...
//!     Then  ...
//! ```

/// A single Gherkin step (Given / When / Then / And / But).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub keyword: StepKeyword,
    pub text: String,
}

/// Gherkin step keywords.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StepKeyword {
    Given,
    When,
    Then,
    And,
    But,
}

impl StepKeyword {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Given => "Given",
            Self::When => "When",
            Self::Then => "Then",
            Self::And => "And",
            Self::But => "But",
        }
    }
}

/// A Gherkin Scenario (or Scenario Outline).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Scenario {
    pub title: String,
    pub steps: Vec<Step>,
    /// `true` when the original source used `Scenario Outline:`.
    pub is_outline: bool,
    /// Optional tags (e.g. `@smoke`, `@regression`).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A complete Gherkin Feature document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GherkinDocument {
    pub feature_title: String,
    pub description: String,
    pub scenarios: Vec<Scenario>,
    /// Background steps shared by all scenarios.
    #[serde(default)]
    pub background: Vec<Step>,
    /// Feature-level tags (e.g. `@feature-tag`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// The file that was the source of this document (e.g. `"D028.docx"`).
    #[allow(dead_code)]
    pub source_file: String,
}

impl GherkinDocument {
    /// Render the document as a valid `.feature` string.
    pub fn to_feature_string(&self) -> String {
        let mut out = String::new();
        if !self.tags.is_empty() {
            out.push_str(&self.tags.join(" "));
            out.push('\n');
        }
        out.push_str(&format!("Feature: {}\n", self.feature_title));
        if !self.description.is_empty() {
            for line in self.description.lines() {
                out.push_str(&format!("  {}\n", line));
            }
            out.push('\n');
        }
        if !self.background.is_empty() {
            out.push_str("  Background:\n");
            for step in &self.background {
                out.push_str(&format!("    {} {}\n", step.keyword.as_str(), step.text));
            }
            out.push('\n');
        }
        for scenario in &self.scenarios {
            if !scenario.tags.is_empty() {
                out.push_str(&format!("  {}\n", scenario.tags.join(" ")));
            }
            let keyword = if scenario.is_outline { "Scenario Outline" } else { "Scenario" };
            out.push_str(&format!("  {}: {}\n", keyword, scenario.title));
            for step in &scenario.steps {
                out.push_str(&format!("    {} {}\n", step.keyword.as_str(), step.text));
            }
            out.push('\n');
        }
        out
    }

    /// Parse an LLM-generated raw text block into a `GherkinDocument`.
    ///
    /// This is a best-effort parser – it handles the most common output shapes
    /// produced by instruction-tuned models without requiring perfect formatting.
    /// Supports: `Feature:`, `Background:`, `Scenario:`, `Scenario Outline:`,
    /// `@tags`, `Examples:` tables, `"""` doc strings, and `#` comments.
    pub fn parse_from_llm_output(raw: &str, source_file: &str) -> Self {
        let mut feature_title = String::from("Generated Feature");
        let mut feature_tags: Vec<String> = Vec::new();
        let mut description_lines: Vec<String> = Vec::new();
        let mut scenarios: Vec<Scenario> = Vec::new();
        let mut background: Vec<Step> = Vec::new();
        let mut current_scenario: Option<Scenario> = None;
        let mut in_description = false;
        let mut in_background = false;
        let mut in_doc_string = false;
        let mut in_examples = false;
        let mut pending_tags: Vec<String> = Vec::new();

        for line in raw.lines() {
            let trimmed = line.trim();

            // Handle doc strings (""" blocks) — skip their content
            if trimmed.starts_with("\"\"\"") {
                in_doc_string = !in_doc_string;
                continue;
            }
            if in_doc_string {
                continue;
            }

            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }

            // Skip Examples table rows (lines starting with |)
            if in_examples {
                if trimmed.starts_with('|') || trimmed.is_empty() {
                    continue;
                }
                in_examples = false;
                // fall through to process the current line
            }

            // Collect @tags
            if trimmed.starts_with('@') {
                let tags: Vec<String> = trimmed
                    .split_whitespace()
                    .filter(|t| t.starts_with('@'))
                    .map(|t| t.to_string())
                    .collect();
                pending_tags.extend(tags);
                continue;
            }

            // Examples: header (for Scenario Outlines)
            if trimmed.starts_with("Examples:") {
                in_examples = true;
                continue;
            }

            if let Some(title) = trimmed.strip_prefix("Feature:") {
                feature_title = title.trim().to_string();
                feature_tags = std::mem::take(&mut pending_tags);
                in_description = true;
                in_background = false;
                continue;
            }

            // Background: block
            if trimmed.starts_with("Background:") {
                if let Some(s) = current_scenario.take() {
                    scenarios.push(s);
                }
                in_background = true;
                in_description = false;
                continue;
            }

            // Scenario Outline must be checked before plain Scenario so that
            // the longer prefix matches first.
            let (scenario_title, is_outline) =
                if let Some(t) = trimmed.strip_prefix("Scenario Outline:") {
                    (Some(t.trim().to_string()), true)
                } else if let Some(t) = trimmed.strip_prefix("Scenario:") {
                    (Some(t.trim().to_string()), false)
                } else {
                    (None, false)
                };

            if let Some(title) = scenario_title {
                if let Some(s) = current_scenario.take() {
                    scenarios.push(s);
                }
                current_scenario = Some(Scenario {
                    title,
                    steps: Vec::new(),
                    is_outline,
                    tags: std::mem::take(&mut pending_tags),
                });
                in_description = false;
                in_background = false;
                continue;
            }

            // Step keywords
            let step_opt = parse_step(trimmed);
            if let Some(step) = step_opt {
                if in_background {
                    background.push(step);
                } else if let Some(ref mut sc) = current_scenario {
                    sc.steps.push(step);
                }
                in_description = false;
                continue;
            }

            // Collect description lines (lines between Feature: and first Scenario:)
            if in_description && !trimmed.is_empty() {
                description_lines.push(trimmed.to_string());
            }
        }

        if let Some(s) = current_scenario.take() {
            scenarios.push(s);
        }

        GherkinDocument {
            feature_title,
            description: description_lines.join("\n"),
            scenarios,
            background,
            tags: feature_tags,
            source_file: source_file.to_string(),
        }
    }
}

/// Try to parse a line as a Gherkin step.
fn parse_step(line: &str) -> Option<Step> {
    let keywords: &[(&str, StepKeyword)] = &[
        ("Given ", StepKeyword::Given),
        ("When ", StepKeyword::When),
        ("Then ", StepKeyword::Then),
        ("And ", StepKeyword::And),
        ("But ", StepKeyword::But),
    ];
    for (prefix, kw) in keywords {
        if let Some(text) = line.strip_prefix(prefix) {
            return Some(Step {
                keyword: kw.clone(),
                text: text.trim().to_string(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_string_rendering() {
        let doc = GherkinDocument {
            feature_title: "User login".to_string(),
            description: "Handles authentication".to_string(),
            scenarios: vec![Scenario {
                title: "Successful login".to_string(),
                is_outline: false,
                tags: Vec::new(),
                steps: vec![
                    Step {
                        keyword: StepKeyword::Given,
                        text: "a registered user".to_string(),
                    },
                    Step {
                        keyword: StepKeyword::When,
                        text: "they submit valid credentials".to_string(),
                    },
                    Step {
                        keyword: StepKeyword::Then,
                        text: "they are redirected to the dashboard".to_string(),
                    },
                ],
            }],
            background: Vec::new(),
            tags: Vec::new(),
            source_file: "login.docx".to_string(),
        };

        let feature = doc.to_feature_string();
        assert!(feature.contains("Feature: User login"));
        // is_outline: false → must render as "Scenario:", not "Scenario Outline:"
        assert!(feature.contains("Scenario: Successful login"));
        assert!(!feature.contains("Scenario Outline:"), "non-outline must not use Scenario Outline:");
        assert!(feature.contains("Given a registered user"));
        assert!(feature.contains("When they submit valid credentials"));
        assert!(feature.contains("Then they are redirected to the dashboard"));
    }

    #[test]
    fn test_parse_from_llm_output() {
        let raw = r#"Feature: Order processing
  Handles order lifecycle

  Scenario: Place an order
    Given a logged-in customer
    When they add items to cart and checkout
    Then an order confirmation is created
"#;
        let doc = GherkinDocument::parse_from_llm_output(raw, "orders.docx");
        assert_eq!(doc.feature_title, "Order processing");
        assert_eq!(doc.scenarios.len(), 1);
        assert_eq!(doc.scenarios[0].steps.len(), 3);
        assert!(!doc.scenarios[0].is_outline);
        assert_eq!(doc.source_file, "orders.docx");
    }

    #[test]
    fn test_parse_scenario_outline() {
        let raw = r#"Feature: D028 - IUS - XML Disconnection Notification
  Scenario Outline: Generate Disconnection Notification even with all 3 llms
    Given a valid disconnection request for <customer>
    When the XML notification is generated
    Then the output matches the expected schema
"#;
        let doc = GherkinDocument::parse_from_llm_output(raw, "D028.docx");
        assert_eq!(doc.scenarios.len(), 1);
        assert!(doc.scenarios[0].is_outline, "should be recognised as Scenario Outline");
        assert_eq!(doc.scenarios[0].steps.len(), 3);
        assert!(doc.to_feature_string().contains("Scenario Outline:"));
    }
}
