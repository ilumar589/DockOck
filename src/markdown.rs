//! Markdown knowledge-base document model.
//!
//! This module defines `MarkdownDocument` — a structured AST representing
//! a rich Markdown file generated from source documents.  It is the output
//! artefact when the user selects `OutputMode::Markdown`.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────
// Data model
// ─────────────────────────────────────────────

/// A comprehensive knowledge-base document generated from source files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownDocument {
    /// Document title derived from the source file name or heading.
    pub title: String,
    /// One-paragraph executive summary.
    pub summary: String,
    /// The source file(s) this was generated from.
    pub source_files: Vec<String>,
    /// Ordered top-level sections.
    pub sections: Vec<Section>,
    /// Cross-references to other documents in the project.
    pub cross_references: Vec<CrossReference>,
}

/// A section within the Markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    /// Section heading text.
    pub heading: String,
    /// The kind of content this section holds.
    pub kind: SectionKind,
    /// Raw Markdown body of the section.
    pub body: String,
    /// Optional sub-sections (for nested headings).
    pub subsections: Vec<Section>,
}

/// Categorises sections so consumers know what data to expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionKind {
    /// Free-form narrative (business rules, process descriptions).
    Narrative,
    /// Database schema: tables, columns, types, constraints.
    DatabaseSchema,
    /// Data model: entities, attributes, relationships.
    DataModel,
    /// Architecture diagram description (from Visio or images).
    ArchitectureDiagram,
    /// Entity-relationship description.
    EntityRelationship,
    /// State machine / lifecycle.
    StateMachine,
    /// Excel-sourced test data or reference tables.
    TestData,
    /// API contracts or service interfaces.
    ApiContract,
    /// Business rules and validation logic.
    BusinessRules,
    /// UI/UX wireframe description.
    UiDescription,
    /// Extracted image/diagram content.
    ImageContent,
}

/// A reference from this document to another project document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossReference {
    /// Name of the referenced document.
    pub target_document: String,
    /// Nature of the relationship (e.g. "depends-on", "extends").
    pub relationship: String,
    /// Brief description of what is shared or depended upon.
    pub description: String,
}

// ─────────────────────────────────────────────
// Rendering
// ─────────────────────────────────────────────

impl MarkdownDocument {
    /// Render the document to a Markdown string.
    pub fn to_markdown_string(&self) -> String {
        let mut out = String::new();

        out.push_str(&format!("# {}\n\n", self.title));
        if !self.summary.is_empty() {
            out.push_str(&format!("## Summary\n\n{}\n\n", self.summary));
        }

        for section in &self.sections {
            out.push_str(&format!("## {}\n\n{}\n\n", section.heading, section.body));
            for sub in &section.subsections {
                out.push_str(&format!("### {}\n\n{}\n\n", sub.heading, sub.body));
            }
        }

        if !self.cross_references.is_empty() {
            out.push_str("## Cross-References\n\n");
            for cr in &self.cross_references {
                out.push_str(&format!(
                    "- **{}** ({}): {}\n",
                    cr.target_document, cr.relationship, cr.description
                ));
            }
            out.push('\n');
        }

        out
    }

    /// Best-effort parse of raw LLM-generated Markdown into a `MarkdownDocument`.
    pub fn parse_from_llm_output(raw: &str, source_file: &str) -> Self {
        let mut title = String::new();
        let mut summary = String::new();
        let mut sections: Vec<Section> = Vec::new();
        let mut cross_references: Vec<CrossReference> = Vec::new();

        // Accumulator for the current ## section
        let mut current_heading: Option<String> = None;
        let mut current_body = String::new();
        // Accumulator for current ### subsection
        let mut current_sub_heading: Option<String> = None;
        let mut current_sub_body = String::new();
        let mut current_subsections: Vec<Section> = Vec::new();

        for line in raw.lines() {
            if let Some(h1) = line.strip_prefix("# ") {
                // Flush any open section first
                if let Some(ref heading) = current_heading {
                    flush_subsection(
                        &mut current_subsections,
                        &mut current_sub_heading,
                        &mut current_sub_body,
                    );
                    sections.push(Section {
                        heading: heading.clone(),
                        kind: kind_from_heading(heading),
                        body: current_body.trim().to_string(),
                        subsections: std::mem::take(&mut current_subsections),
                    });
                    current_body.clear();
                    current_heading = None;
                }
                if title.is_empty() {
                    title = h1.trim().to_string();
                }
                continue;
            }

            if let Some(h2) = line.strip_prefix("## ") {
                let h2 = h2.trim().to_string();

                // Flush previous ## section
                if let Some(ref heading) = current_heading {
                    flush_subsection(
                        &mut current_subsections,
                        &mut current_sub_heading,
                        &mut current_sub_body,
                    );

                    if heading.eq_ignore_ascii_case("Cross-References") {
                        // Parse cross-refs from the accumulated body
                        cross_references = parse_cross_references(&current_body);
                    } else {
                        sections.push(Section {
                            heading: heading.clone(),
                            kind: kind_from_heading(heading),
                            body: current_body.trim().to_string(),
                            subsections: std::mem::take(&mut current_subsections),
                        });
                    }
                    current_body.clear();
                }

                // Check if this is the Summary section
                if h2.eq_ignore_ascii_case("Summary") {
                    // We'll collect body into `summary` via the heading name later
                    current_heading = Some(h2);
                } else {
                    current_heading = Some(h2);
                }
                continue;
            }

            if let Some(h3) = line.strip_prefix("### ") {
                flush_subsection(
                    &mut current_subsections,
                    &mut current_sub_heading,
                    &mut current_sub_body,
                );
                current_sub_heading = Some(h3.trim().to_string());
                continue;
            }

            // Accumulate body text
            if current_sub_heading.is_some() {
                current_sub_body.push_str(line);
                current_sub_body.push('\n');
            } else {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }

        // Flush final section
        if let Some(ref heading) = current_heading {
            flush_subsection(
                &mut current_subsections,
                &mut current_sub_heading,
                &mut current_sub_body,
            );
            if heading.eq_ignore_ascii_case("Cross-References") {
                cross_references = parse_cross_references(&current_body);
            } else if heading.eq_ignore_ascii_case("Summary") {
                summary = current_body.trim().to_string();
            } else {
                sections.push(Section {
                    heading: heading.clone(),
                    kind: kind_from_heading(heading),
                    body: current_body.trim().to_string(),
                    subsections: current_subsections,
                });
            }
        }

        // If summary was captured as a section body, extract it
        if summary.is_empty() {
            if let Some(idx) = sections.iter().position(|s| s.heading.eq_ignore_ascii_case("Summary")) {
                summary = sections.remove(idx).body;
            }
        }

        // Fallback title
        if title.is_empty() {
            title = source_file
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(source_file)
                .to_string();
        }

        Self {
            title,
            summary,
            source_files: vec![source_file.to_string()],
            sections,
            cross_references,
        }
    }
}

/// Generate a project-level index document that maps relationships between
/// all generated Markdown documents.
pub fn generate_project_index(
    documents: &HashMap<String, MarkdownDocument>,
) -> MarkdownDocument {
    let mut inventory_rows = String::from("| Document | Source Files | Sections |\n|---|---|---|\n");
    let mut all_cross_refs: Vec<String> = Vec::new();
    let mut entity_counts: HashMap<String, Vec<String>> = HashMap::new();

    let mut sorted_keys: Vec<&String> = documents.keys().collect();
    sorted_keys.sort();

    for key in &sorted_keys {
        let doc = &documents[*key];
        let sources = doc.source_files.join(", ");
        let section_count = doc.sections.len();
        inventory_rows.push_str(&format!(
            "| {} | {} | {} |\n",
            doc.title, sources, section_count
        ));

        for cr in &doc.cross_references {
            all_cross_refs.push(format!(
                "- **{}** → **{}** ({}): {}",
                doc.title, cr.target_document, cr.relationship, cr.description
            ));
        }

        // Collect entity names from DataModel / EntityRelationship sections
        for section in &doc.sections {
            if section.kind == SectionKind::DataModel
                || section.kind == SectionKind::EntityRelationship
            {
                // Simple heuristic: look for ### headings or **bold** names
                for line in section.body.lines() {
                    if let Some(name) = line.strip_prefix("### ") {
                        entity_counts
                            .entry(name.trim().to_string())
                            .or_default()
                            .push(doc.title.clone());
                    }
                }
            }
        }
    }

    let cross_ref_body = if all_cross_refs.is_empty() {
        "No cross-references found.".to_string()
    } else {
        all_cross_refs.join("\n")
    };

    let shared: Vec<String> = entity_counts
        .iter()
        .filter(|(_, docs)| docs.len() > 1)
        .map(|(name, docs)| format!("- **{}**: appears in {}", name, docs.join(", ")))
        .collect();
    let shared_body = if shared.is_empty() {
        "No shared entities detected across documents.".to_string()
    } else {
        shared.join("\n")
    };

    MarkdownDocument {
        title: "Project Knowledge Base Index".to_string(),
        summary: format!(
            "Index of {} knowledge-base documents generated from project source files.",
            documents.len()
        ),
        source_files: vec!["_INDEX.md".to_string()],
        sections: vec![
            Section {
                heading: "Document Inventory".to_string(),
                kind: SectionKind::Narrative,
                body: inventory_rows,
                subsections: Vec::new(),
            },
            Section {
                heading: "Cross-Reference Map".to_string(),
                kind: SectionKind::Narrative,
                body: cross_ref_body,
                subsections: Vec::new(),
            },
            Section {
                heading: "Shared Entities".to_string(),
                kind: SectionKind::Narrative,
                body: shared_body,
                subsections: Vec::new(),
            },
        ],
        cross_references: Vec::new(),
    }
}

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

/// Map a heading string to an appropriate `SectionKind`.
fn kind_from_heading(heading: &str) -> SectionKind {
    let lower = heading.to_lowercase();
    if lower.contains("database") || lower.contains("schema") {
        SectionKind::DatabaseSchema
    } else if lower.contains("data model") {
        SectionKind::DataModel
    } else if lower.contains("architecture") {
        SectionKind::ArchitectureDiagram
    } else if lower.contains("entity relationship") || lower.contains("er diagram") {
        SectionKind::EntityRelationship
    } else if lower.contains("state machine") || lower.contains("lifecycle") {
        SectionKind::StateMachine
    } else if lower.contains("business rule") {
        SectionKind::BusinessRules
    } else if lower.contains("test data") {
        SectionKind::TestData
    } else if lower.contains("process flow") {
        SectionKind::Narrative
    } else if lower.contains("ui") || lower.contains("screen") || lower.contains("specification") {
        SectionKind::UiDescription
    } else if lower.contains("visio") || lower.contains("diagram") || lower.contains("image") {
        SectionKind::ImageContent
    } else if lower.contains("excel") || lower.contains("reference data") {
        SectionKind::TestData
    } else if lower.contains("api") || lower.contains("contract") {
        SectionKind::ApiContract
    } else {
        SectionKind::Narrative
    }
}

/// Flush the current subsection accumulator into the subsections vec.
fn flush_subsection(
    subsections: &mut Vec<Section>,
    heading: &mut Option<String>,
    body: &mut String,
) {
    if let Some(h) = heading.take() {
        subsections.push(Section {
            heading: h.clone(),
            kind: kind_from_heading(&h),
            body: body.trim().to_string(),
            subsections: Vec::new(),
        });
        body.clear();
    }
}

/// Parse cross-references from bullet lines like:
/// `- **TargetDoc** (relationship): description`
fn parse_cross_references(body: &str) -> Vec<CrossReference> {
    let mut refs = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("- ") {
            continue;
        }
        let line = &line[2..];
        // Try pattern: **target** (rel): desc
        if let Some(rest) = line.strip_prefix("**") {
            if let Some(end_bold) = rest.find("**") {
                let target = rest[..end_bold].to_string();
                let remainder = rest[end_bold + 2..].trim();
                let (relationship, description) = if let Some(paren_start) = remainder.find('(') {
                    if let Some(paren_end) = remainder.find(')') {
                        let rel = remainder[paren_start + 1..paren_end].to_string();
                        let desc = remainder[paren_end + 1..].trim_start_matches(':').trim().to_string();
                        (rel, desc)
                    } else {
                        ("references".to_string(), remainder.to_string())
                    }
                } else if let Some(colon) = remainder.find(':') {
                    let rel = remainder[..colon].trim_matches(&[' ', '—', '-'][..]).to_string();
                    let desc = remainder[colon + 1..].trim().to_string();
                    (if rel.is_empty() { "references".to_string() } else { rel }, desc)
                } else {
                    ("references".to_string(), remainder.to_string())
                };
                refs.push(CrossReference {
                    target_document: target,
                    relationship,
                    description,
                });
            }
        }
    }
    refs
}
