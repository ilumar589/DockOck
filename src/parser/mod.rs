//! File parsing module.
//!
//! Supports three document formats:
//! - **Word** (`.docx`)  – parsed by [`word::parse`]
//! - **Excel** (`.xlsx`) – parsed by [`excel::parse`]
//! - **Visio** (`.vsdx`) – parsed by [`visio::parse`]
//!
//! Every parser returns a plain `String` containing the extracted text that is
//! later handed to the LLM for Gherkin generation.

pub mod excel;
pub mod visio;
pub mod word;

use anyhow::{Result, anyhow};
use std::path::Path;

/// Dispatch parsing to the correct sub-module based on the file extension.
///
/// Returns `(file_type_label, extracted_text)`.
pub fn parse_file(path: &Path) -> Result<(String, String)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("docx") => {
            let text = word::parse(path)?;
            Ok(("Word".to_string(), text))
        }
        Some("xlsx") | Some("xls") | Some("xlsm") | Some("xlsb") | Some("ods") => {
            let text = excel::parse(path)?;
            Ok(("Excel".to_string(), text))
        }
        Some("vsdx") | Some("vsd") | Some("vsdm") => {
            let text = visio::parse(path)?;
            Ok(("Visio".to_string(), text))
        }
        _ => Err(anyhow!(
            "Unsupported file type: {}",
            path.display()
        )),
    }
}
