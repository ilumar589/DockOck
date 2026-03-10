//! Parser for Microsoft Word `.docx` files.
//!
//! A `.docx` file is a ZIP archive containing XML files.  The main text lives
//! in `word/document.xml`.  This parser unzips that entry and extracts all
//! `<w:t>` (text run) elements, preserving paragraph breaks.

use anyhow::{Context, Result};
use std::io::{Cursor, Read};
use std::path::Path;

/// Extract all readable text from a `.docx` file.
pub fn parse(path: &Path) -> Result<String> {
    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .with_context(|| format!("Failed to open ZIP archive: {}", path.display()))?;

    // Read the main document XML
    let xml = read_zip_entry(&mut archive, "word/document.xml")
        .with_context(|| "Failed to read word/document.xml from archive")?;

    extract_text_from_xml(&xml)
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

/// Walk the `<w:t>` nodes in the document XML and collect their text.
///
/// Paragraph (`<w:p>`) boundaries are converted to newlines so that the output
/// is readable plain text.
fn extract_text_from_xml(xml: &str) -> Result<String> {
    let doc = roxmltree::Document::parse(xml)
        .with_context(|| "Failed to parse word/document.xml as XML")?;

    let mut output = String::new();
    let mut prev_para = None::<roxmltree::NodeId>;

    for node in doc.descendants() {
        if node.is_element() {
            let local = node.tag_name().name();

            if local == "p" {
                // Each new paragraph gets a newline separator before it
                // (but not before the very first paragraph).
                if prev_para.is_some() {
                    output.push('\n');
                }
                prev_para = Some(node.id());
            }

            if local == "t" {
                if let Some(text) = node.text() {
                    if !text.trim().is_empty() {
                        output.push_str(text);
                        output.push(' ');
                    }
                }
            }
        }
    }

    Ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_from_xml() {
        // Minimal document XML fragment
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
}
