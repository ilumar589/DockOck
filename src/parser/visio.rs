//! Parser for Microsoft Visio `.vsdx` files.
//!
//! A `.vsdx` file is a ZIP archive containing XML pages under `visio/pages/`.
//! Each page file contains `<VisioDocument>` XML with shape data.  Shape text
//! lives inside `<Text>` elements; shape names/labels are in `<Cell>` elements
//! with `N="Label"` or `N="Name"`.
//!
//! This parser extracts shape text, labels and names from every page so the
//! LLM has a meaningful description of the diagram contents.

use anyhow::{Context, Result};
use std::io::{Cursor, Read};
use std::path::Path;

/// Extract all readable text from a `.vsdx` file.
pub fn parse(path: &Path) -> Result<String> {
    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .with_context(|| format!("Failed to open ZIP archive: {}", path.display()))?;

    // Collect page file names first to avoid borrowing issues
    let page_names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive.by_index(i).ok().and_then(|entry| {
                let name = entry.name().to_string();
                if name.starts_with("visio/pages/page") && name.ends_with(".xml") {
                    Some(name)
                } else {
                    None
                }
            })
        })
        .collect();

    let mut output = String::new();

    for page_name in &page_names {
        let page_number = page_name
            .trim_start_matches("visio/pages/page")
            .trim_end_matches(".xml");

        output.push_str(&format!("=== Page {} ===\n", page_number));

        let xml = read_zip_entry(&mut archive, page_name)
            .with_context(|| format!("Failed to read page '{}'", page_name))?;

        let page_text = extract_text_from_page(&xml)
            .with_context(|| format!("Failed to parse page '{}'", page_name))?;

        output.push_str(&page_text);
        output.push('\n');
    }

    // Fall back to the master shapes if no pages were found
    if page_names.is_empty() {
        output.push_str("(No Visio page files found in archive)\n");
    }

    Ok(output.trim().to_string())
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

/// Extract shape labels and text content from a single Visio page XML.
fn extract_text_from_page(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)
        .with_context(|| "Failed to parse Visio page XML")?;

    let mut output = String::new();

    for node in doc.descendants() {
        if !node.is_element() {
            continue;
        }
        let local = node.tag_name().name();

        // Shape label / name from Cell elements
        if local == "Cell" {
            let attr_n = node.attribute("N").unwrap_or("");
            if matches!(attr_n, "Label" | "Name" | "Comment") {
                if let Some(v) = node.attribute("V") {
                    if !v.trim().is_empty() {
                        output.push_str(&format!("[{}] {}\n", attr_n, v.trim()));
                    }
                }
            }
        }

        // Shape text content
        if local == "Text" {
            let text: String = node
                .descendants()
                .filter(|n| n.is_text())
                .filter_map(|n| n.text())
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.trim().to_string();
            if !text.is_empty() {
                output.push_str(&format!("{}\n", text));
            }
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_from_page() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<VisioDocument xmlns="http://schemas.microsoft.com/office/visio/2012/main">
  <Shapes>
    <Shape ID="1">
      <Cell N="Label" V="Start Process"/>
      <Text>Begin the workflow here</Text>
    </Shape>
    <Shape ID="2">
      <Cell N="Name" V="Decision Gate"/>
      <Text>Is approval required?</Text>
    </Shape>
  </Shapes>
</VisioDocument>"#;

        let text = extract_text_from_page(xml).unwrap();
        assert!(text.contains("Start Process"));
        assert!(text.contains("Begin the workflow here"));
        assert!(text.contains("Decision Gate"));
        assert!(text.contains("Is approval required?"));
    }
}
