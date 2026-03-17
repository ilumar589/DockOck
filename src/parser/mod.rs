//! File parsing module.
//!
//! Supports three document formats:
//! - **Word** (`.docx`)  – parsed by [`word::parse`]
//! - **Excel** (`.xlsx`) – parsed by [`excel::parse`]
//! - **Visio** (`.vsdx`) – parsed by [`visio::parse`]
//!
//! Every parser returns a [`ParseResult`] containing extracted text and any
//! embedded images that can be described by a vision model.

pub mod excel;
pub mod visio;
pub mod word;

use anyhow::{Result, anyhow};
use std::path::Path;

/// Whether a file produces its own Gherkin or only provides context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileRole {
    /// Transformed into Gherkin (Word documents).
    Primary,
    /// Parsed for context only; no Gherkin output (Excel, Visio).
    Context,
}

impl FileRole {
    /// Derive the role from a file extension.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "docx" => Self::Primary,
            _ => Self::Context,
        }
    }
}

/// File extensions accepted by the parsers.
pub const ACCEPTED_EXTENSIONS: &[&str] = &[
    "docx", "xlsx", "xls", "xlsm", "xlsb", "ods", "vsdx", "vsd", "vsdm",
];

/// An image extracted from a document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtractedImage {
    /// Human-readable label (alt-text, filename, etc.)
    pub label: String,
    /// Raw image bytes
    pub data: Vec<u8>,
    /// MIME type (e.g. "image/png", "image/jpeg")
    #[allow(dead_code)]
    pub content_type: String,
}

/// Result of parsing a document file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParseResult {
    /// Human-readable file type label ("Word", "Excel", "Visio")
    pub file_type: String,
    /// Extracted text content
    pub text: String,
    /// Extracted images that can be sent to a vision model for description
    pub images: Vec<ExtractedImage>,
    /// Whether this file produces Gherkin output or only provides context.
    pub role: FileRole,
}

/// Determine MIME type from file extension.
pub fn mime_from_extension(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png".to_string(),
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "gif" => "image/gif".to_string(),
        "bmp" => "image/bmp".to_string(),
        "webp" => "image/webp".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Check if the image format can be processed by vision models.
pub fn is_vision_compatible(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/bmp" | "image/webp"
    )
}

/// Dispatch parsing to the correct sub-module based on the file extension.
pub fn parse_file(path: &Path) -> Result<ParseResult> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("docx") => {
            let mut result = word::parse(path)?;
            result.role = FileRole::Primary;
            Ok(result)
        }
        Some("xlsx") | Some("xls") | Some("xlsm") | Some("xlsb") | Some("ods") => {
            let text = excel::parse(path)?;
            Ok(ParseResult {
                file_type: "Excel".to_string(),
                text,
                images: Vec::new(),
                role: FileRole::Context,
            })
        }
        Some("vsdx") | Some("vsd") | Some("vsdm") => {
            let mut result = visio::parse(path)?;
            result.role = FileRole::Context;
            Ok(result)
        }
        _ => Err(anyhow!(
            "Unsupported file type: {}",
            path.display()
        )),
    }
}
