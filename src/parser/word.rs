//! Parser for Microsoft Word `.docx` files.
//!
//! A `.docx` file is a ZIP archive containing XML files.  The main text lives
//! in `word/document.xml`.  This parser unzips that entry and extracts:
//!
//! * **Paragraphs** – plain text, with Markdown-style heading markers
//!   (`# ` / `## ` / `### `) for `Title`, `Heading1`, `Heading2` … styles.
//! * **Tables** – rendered as pipe-delimited rows (`| cell | cell |`) so the
//!   LLM receives structured tabular data instead of a jumbled text stream.
//! * **Images** – represented by an `[Image: <alt-text>]` placeholder extracted
//!   from the `<wp:docPr descr>` (or `name`) attribute of every drawing.  The
//!   pixel data is not sent to the LLM, only the human-readable description.

use anyhow::{Context, Result};
use std::io::{Cursor, Read};
use std::path::Path;

use super::{ExtractedImage, ParseResult, is_vision_compatible, mime_from_extension};

/// Extract all readable text and embedded images from a `.docx` file.
#[tracing::instrument(name = "parser.word", skip(path), fields(file_name = %path.display()))]
pub fn parse(path: &Path) -> Result<ParseResult> {
    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .with_context(|| format!("Failed to open ZIP archive: {}", path.display()))?;

    // Read the main document XML
    let xml = read_zip_entry(&mut archive, "word/document.xml")
        .with_context(|| "Failed to read word/document.xml from archive")?;

    let text = extract_text_from_xml(&xml)?;
    let images = extract_images_from_archive(&mut archive);

    Ok(ParseResult {
        file_type: "Word".to_string(),
        text,
        images,
    })
}

/// Read a named entry from a ZIP archive and return its contents as a UTF-8 string.
fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("Entry '{}' not found in archive", name))?;

    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .with_context(|| format!("Failed to read entry '{}'", name))?;

    String::from_utf8(buf).with_context(|| format!("Entry '{}' is not valid UTF-8", name))
}

// ─────────────────────────────────────────────────────────────────────────────
// Top-level XML walker
// ─────────────────────────────────────────────────────────────────────────────

/// Walk the body of `word/document.xml` and produce readable plain text
/// with a structural outline prepended.
///
/// The output begins with a `=== DOCUMENT STRUCTURE ===` header that lists
/// all top-level sections (headings) together with paragraph and table counts.
/// This gives the LLM an overview before diving into the content.
///
/// Top-level body children are processed in document order:
/// * `<w:p>`   → [`extract_paragraph`]
/// * `<w:tbl>` → [`extract_table`]
///
/// Nested structures (e.g. a paragraph inside a table cell) are handled
/// recursively by the per-element helpers.
fn extract_text_from_xml(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)
        .with_context(|| "Failed to parse word/document.xml as XML")?;

    let body = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "body")
        .ok_or_else(|| anyhow::anyhow!("No <w:body> element found in document.xml"))?;

    // ── First pass: collect structure metadata ──
    struct SectionInfo {
        heading: String,
        paragraphs: usize,
        tables: usize,
    }

    let mut sections: Vec<SectionInfo> = Vec::new();
    let mut title: Option<String> = None;
    // Track items before the first heading
    let mut pre_heading_paragraphs = 0usize;
    let mut pre_heading_tables = 0usize;

    // Collect effective body children, skipping tracked-change deletions and
    // unwrapping insertion wrappers so we see only the final accepted content.
    let effective_children: Vec<roxmltree::Node> = body
        .children()
        .filter(|n| n.is_element())
        .flat_map(|child| {
            let tag = child.tag_name().name();
            match tag {
                // Deleted / moved-from content — skip entirely
                "del" | "moveFrom" => Vec::new(),
                // Inserted / moved-to wrappers — unwrap to inner elements
                "ins" | "moveTo" => child
                    .children()
                    .filter(|n| n.is_element())
                    .collect::<Vec<_>>(),
                // Normal elements — keep as-is
                _ => vec![child],
            }
        })
        .collect();

    for child in &effective_children {
        match child.tag_name().name() {
            "p" => {
                let style = paragraph_style(&child);
                let is_heading = matches!(
                    style.as_deref(),
                    Some("Title") | Some("title")
                ) || style.as_ref().is_some_and(|s| {
                    s.to_ascii_lowercase().starts_with("heading")
                        || matches!(s.as_str(), "1" | "2" | "3")
                });

                if is_heading {
                    let text = extract_paragraph(&child);
                    if !text.is_empty() {
                        // Capture the title specifically
                        if matches!(style.as_deref(), Some("Title") | Some("title"))
                            && title.is_none()
                        {
                            title = Some(text.trim_start_matches('#').trim().to_string());
                        }
                        sections.push(SectionInfo {
                            heading: text.trim_start_matches('#').trim().to_string(),
                            paragraphs: 0,
                            tables: 0,
                        });
                    }
                } else {
                    let text = extract_paragraph(&child);
                    if !text.is_empty() {
                        if let Some(last) = sections.last_mut() {
                            last.paragraphs += 1;
                        } else {
                            pre_heading_paragraphs += 1;
                        }
                    }
                }
            }
            "tbl" => {
                if let Some(last) = sections.last_mut() {
                    last.tables += 1;
                } else {
                    pre_heading_tables += 1;
                }
            }
            _ => {}
        }
    }

    // ── Build structural outline ──
    let mut output = String::new();
    if !sections.is_empty() {
        output.push_str("=== DOCUMENT STRUCTURE ===\n");
        if let Some(ref t) = title {
            output.push_str(&format!("Title: {}\n", t));
        }
        if pre_heading_paragraphs > 0 || pre_heading_tables > 0 {
            output.push_str(&format!(
                "  (preamble: {} paragraphs, {} tables)\n",
                pre_heading_paragraphs, pre_heading_tables
            ));
        }
        output.push_str("Sections:\n");
        for (i, sec) in sections.iter().enumerate() {
            output.push_str(&format!(
                "  {}. {} (paragraphs: {}, tables: {})\n",
                i + 1,
                sec.heading,
                sec.paragraphs,
                sec.tables,
            ));
        }
        output.push_str("\n=== SECTION CONTENT ===\n\n");
    }

    // ── Second pass: emit content ──
    // Reuse the same effective children (tracked-change deletions already removed).
    for child in &effective_children {
        match child.tag_name().name() {
            "p" => {
                let text = extract_paragraph(&child);
                if !text.is_empty() {
                    output.push_str(&text);
                    output.push('\n');
                }
            }
            "tbl" => {
                let table = extract_table(&child);
                if !table.is_empty() {
                    output.push('\n');
                    output.push_str(&table);
                    output.push('\n');
                }
            }
            _ => {}
        }
    }

    Ok(output.trim().to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tracked-changes helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` if `node` (or any of its ancestors up to `root`) sits inside
/// a tracked-change deletion element.  In OOXML these are:
///
/// * `<w:del>`       – deleted text shown in "All Markup" view
/// * `<w:moveFrom>`  – original location of moved text
///
/// Text inside `<w:ins>` and `<w:moveTo>` is the *accepted* version and is
/// intentionally kept.
fn is_inside_deletion(node: &roxmltree::Node) -> bool {
    let mut cursor = *node;
    while let Some(parent) = cursor.parent() {
        if parent.is_element() {
            match parent.tag_name().name() {
                "del" | "moveFrom" => return true,
                _ => {}
            }
        }
        cursor = parent;
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Paragraph helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Return the `w:val` of the first `<w:pStyle>` found inside a paragraph's
/// `<w:pPr>` element, if any.
fn paragraph_style(para: &roxmltree::Node) -> Option<String> {
    para.children()
        .filter(|n| n.is_element() && n.tag_name().name() == "pPr")
        .flat_map(|ppr| ppr.children())
        .filter(|n| n.is_element() && n.tag_name().name() == "pStyle")
        .find_map(|n| {
            // The style value lives in the `w:val` attribute; roxmltree exposes
            // it via the local name "val" regardless of namespace prefix.
            n.attributes()
                .find(|a: &roxmltree::Attribute| a.name() == "val")
                .map(|a| a.value().to_string())
        })
}

/// Collect the text content (and image placeholders) from a single paragraph.
///
/// The returned string already includes a Markdown-style heading prefix when
/// the paragraph style starts with `"Heading"` or equals `"Title"`.
fn extract_paragraph(para: &roxmltree::Node) -> String {
    let style = paragraph_style(para);

    let mut parts: Vec<String> = Vec::new();

    for node in para.descendants() {
        if !node.is_element() {
            continue;
        }
        // Skip content inside tracked-change deletions (w:del, w:moveFrom)
        if is_inside_deletion(&node) {
            continue;
        }
        match node.tag_name().name() {
            // Text run
            "t" => {
                if let Some(text) = node.text() {
                    if !text.trim().is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            // Drawing / image — extract the human-readable description
            "docPr" => {
                let descr = node.attribute("descr").unwrap_or("").trim().to_string();
                let name = node.attribute("name").unwrap_or("").trim().to_string();
                let label = if !descr.is_empty() { descr } else { name };
                if !label.is_empty() {
                    parts.push(format!("[Image: {}]", label));
                }
            }
            _ => {}
        }
    }

    if parts.is_empty() {
        return String::new();
    }

    // Join with a space so that adjacent text runs don't run together.
    // The trim() removes any leading/trailing whitespace introduced by the join.
    let combined = parts.join(" ").trim().to_string();

    // Apply Markdown-style heading prefix based on paragraph style
    match style.as_deref() {
        Some("Title") | Some("title") => format!("# {}", combined),
        Some(s) if s.eq_ignore_ascii_case("Heading1") || s == "1" => {
            format!("# {}", combined)
        }
        Some(s) if s.eq_ignore_ascii_case("Heading2") || s == "2" => {
            format!("## {}", combined)
        }
        Some(s) if s.eq_ignore_ascii_case("Heading3") || s == "3" => {
            format!("### {}", combined)
        }
        Some(s) if s.to_ascii_lowercase().starts_with("heading") => {
            format!("#### {}", combined)
        }
        _ => combined,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Table helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Collect all text (and image placeholders) from a single table cell.
///
/// Paragraphs within the cell are joined with a space; nested tables produce
/// their own inline text without extra delimiters.
fn collect_cell_text(cell: &roxmltree::Node) -> String {
    let mut parts: Vec<String> = Vec::new();

    for node in cell.descendants() {
        if !node.is_element() {
            continue;
        }
        // Skip content inside tracked-change deletions (w:del, w:moveFrom)
        if is_inside_deletion(&node) {
            continue;
        }
        match node.tag_name().name() {
            "t" => {
                if let Some(text) = node.text() {
                    parts.push(text.to_string());
                }
            }
            "docPr" => {
                let descr = node.attribute("descr").unwrap_or("").trim().to_string();
                let name = node.attribute("name").unwrap_or("").trim().to_string();
                let label = if !descr.is_empty() { descr } else { name };
                if !label.is_empty() {
                    parts.push(format!("[Image: {}]", label));
                }
            }
            _ => {}
        }
    }

    // Join with a space so that adjacent text runs don't run together.
    parts.join(" ").trim().to_string()
}

/// Render a `<w:tbl>` element as pipe-delimited rows.
///
/// Empty rows (all cells blank) are skipped.  The result looks like:
///
/// ```text
/// | Header A | Header B | Header C |
/// | value 1  | value 2  | value 3  |
/// ```
fn extract_table(table: &roxmltree::Node) -> String {
    let mut rows_output: Vec<String> = Vec::new();

    for row in table
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "tr")
    {
        let cells: Vec<String> = row
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "tc")
            .map(|cell| collect_cell_text(&cell))
            .collect();

        if !cells.is_empty() && cells.iter().any(|c| !c.is_empty()) {
            rows_output.push(format!("| {} |", cells.join(" | ")));
        }
    }

    rows_output.join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// Image extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract all images from the `word/media/` directory in the archive.
fn extract_images_from_archive<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Vec<ExtractedImage> {
    let mut images = Vec::new();

    let media_entries: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive.by_index(i).ok().and_then(|entry| {
                let name = entry.name().to_string();
                if name.starts_with("word/media/") && !name.ends_with('/') {
                    Some(name)
                } else {
                    None
                }
            })
        })
        .collect();

    for entry_name in media_entries {
        if let Ok(mut entry) = archive.by_name(&entry_name) {
            let mut buf = Vec::new();
            if entry.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                let file_name = entry_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry_name)
                    .to_string();

                let content_type = mime_from_extension(&file_name);

                if is_vision_compatible(&content_type) {
                    images.push(ExtractedImage {
                        label: file_name,
                        data: buf,
                        content_type,
                    });
                }
            }
        }
    }

    images
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic paragraph text ──────────────────────────────────────────────

    #[test]
    fn test_extract_text_from_xml_paragraphs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Hello</w:t></w:r>
      <w:r><w:t xml:space="preserve"> World</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Second paragraph</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(text.contains("Second paragraph"));
    }

    // ── Heading styles ────────────────────────────────────────────────────

    #[test]
    fn test_heading_style_prefix() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Introduction</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
      <w:r><w:t>Background</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Normal text here</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(text.contains("# Introduction"), "H1 should get single #");
        assert!(text.contains("## Background"), "H2 should get double ##");
        assert!(text.contains("Normal text here"));
    }

    // ── Table extraction ──────────────────────────────────────────────────

    #[test]
    fn test_table_extraction() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Field A</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>123</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(text.contains("| Name | Value |"), "header row should be pipe-delimited");
        assert!(text.contains("| Field A | 123 |"), "data row should be pipe-delimited");
    }

    #[test]
    fn test_empty_table_rows_are_skipped() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:tbl>
      <w:tr>
        <w:tc><w:p></w:p></w:tc>
        <w:tc><w:p></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Data</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        // Empty row should not produce a pipe line
        let pipe_lines: Vec<&str> = text.lines().filter(|l| l.starts_with('|')).collect();
        assert_eq!(pipe_lines.len(), 1, "only the non-empty row should appear");
        assert!(text.contains("| Data | Value |"));
    }

    // ── Image alt-text ────────────────────────────────────────────────────

    #[test]
    fn test_image_alt_text_extraction() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document
  xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
  xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
  <w:body>
    <w:p>
      <w:r>
        <w:drawing>
          <wp:inline>
            <wp:docPr id="1" name="Figure 1" descr="Process flow diagram showing the disconnection steps"/>
          </wp:inline>
        </w:drawing>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(
            text.contains("[Image: Process flow diagram showing the disconnection steps]"),
            "descr attribute should be used as alt text: {text}"
        );
    }

    #[test]
    fn test_image_falls_back_to_name_when_no_descr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document
  xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
  xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
  <w:body>
    <w:p>
      <w:r>
        <w:drawing>
          <wp:inline>
            <wp:docPr id="2" name="Architecture diagram"/>
          </wp:inline>
        </w:drawing>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(
            text.contains("[Image: Architecture diagram]"),
            "name attribute should be used when descr is absent: {text}"
        );
    }

    // ── Mixed content ─────────────────────────────────────────────────────

    #[test]
    fn test_mixed_paragraphs_tables_images() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document
  xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
  xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Service Description</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Trigger</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Customer request</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
    <w:p>
      <w:r>
        <w:drawing>
          <wp:inline>
            <wp:docPr id="1" name="fig1" descr="Conceptual data model"/>
          </wp:inline>
        </w:drawing>
      </w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let text = extract_text_from_xml(xml).unwrap();
        assert!(text.contains("# Service Description"));
        assert!(text.contains("| Trigger | Customer request |"));
        assert!(text.contains("[Image: Conceptual data model]"));
    }
}
