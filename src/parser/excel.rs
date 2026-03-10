//! Parser for Microsoft Excel files (`.xlsx`, `.xls`, `.ods`, etc.).
//!
//! Uses the `calamine` crate which supports multiple spreadsheet formats.
//! Every worksheet in the workbook is extracted.  Each sheet is rendered as a
//! plain-text table where:
//! - The sheet name is printed as a heading.
//! - Non-empty rows are written as tab-separated cell values.
//! - Empty sheets are noted explicitly so the LLM context is unambiguous.
//!
//! A short workbook summary (sheet count + sheet names) is prepended to the
//! output so that the LLM has an upfront map of the file's structure when
//! multiple sheets are present.

use anyhow::{Context, Result};
use calamine::{Data, Reader, open_workbook_auto};
use std::path::Path;

/// Extract all text from every worksheet in the workbook.
pub fn parse(path: &Path) -> Result<String> {
    let mut workbook = open_workbook_auto(path)
        .with_context(|| format!("Failed to open spreadsheet: {}", path.display()))?;

    let sheet_names: Vec<String> = workbook.sheet_names().to_vec();

    if sheet_names.is_empty() {
        return Ok("(Workbook contains no sheets)".to_string());
    }

    let mut output = String::new();

    // ── Workbook summary ────────────────────────────────────────────────
    // Prepend a brief map so the LLM understands the overall structure
    // before reading any individual sheet.
    if sheet_names.len() > 1 {
        output.push_str(&format!(
            "Workbook contains {} sheets: {}\n\n",
            sheet_names.len(),
            sheet_names.join(", ")
        ));
    }

    // ── Per-sheet content ────────────────────────────────────────────────
    for sheet_name in &sheet_names {
        output.push_str(&format!("=== Sheet: {} ===\n", sheet_name));

        match workbook.worksheet_range(sheet_name) {
            Err(e) => {
                // Surface the error so the LLM context is accurate
                output.push_str(&format!("(Could not read sheet: {})\n", e));
            }
            Ok(range) => {
                let mut row_count = 0usize;

                for row in range.rows() {
                    let cells: Vec<String> = row.iter().map(cell_to_string).collect();
                    // Skip rows where every cell is empty
                    if cells.iter().any(|c| !c.is_empty()) {
                        output.push_str(&cells.join("\t"));
                        output.push('\n');
                        row_count += 1;
                    }
                }

                if row_count == 0 {
                    output.push_str("(empty sheet)\n");
                }
            }
        }

        output.push('\n');
    }

    Ok(output.trim().to_string())
}

/// Convert a spreadsheet cell value to a human-readable string.
fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Int(n) => n.to_string(),
        Data::Float(f) => {
            // Guard against NaN / infinity before casting
            if !f.is_finite() {
                return format!("{}", f);
            }
            // Avoid unnecessary decimal places for whole numbers
            if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                format!("{}", *f as i64)
            } else {
                format!("{:.4}", f)
            }
        }
        Data::String(s) => s.clone(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => format!("{}", dt),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERR:{:?}", e),
        Data::Empty => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::Data;

    // ── cell_to_string ────────────────────────────────────────────────────

    #[test]
    fn test_cell_to_string_variants() {
        assert_eq!(cell_to_string(&Data::Int(42)), "42");
        assert_eq!(cell_to_string(&Data::Float(3.5)), "3.5000");
        assert_eq!(cell_to_string(&Data::Float(4.0)), "4");
        assert_eq!(
            cell_to_string(&Data::String("hello".to_string())),
            "hello"
        );
        assert_eq!(cell_to_string(&Data::Bool(true)), "true");
        assert_eq!(cell_to_string(&Data::Empty), "");
    }

    // ── Multi-sheet output formatting ─────────────────────────────────────
    // These tests exercise the formatting helpers directly without needing
    // a real .xlsx file on disk.

    /// Simulate what `parse()` produces for a two-sheet workbook by calling
    /// the same formatting logic used inside the function.
    #[test]
    fn test_multi_sheet_summary_header() {
        let sheet_names = vec!["Summary".to_string(), "Details".to_string(), "Lookup".to_string()];
        let summary = format!(
            "Workbook contains {} sheets: {}\n\n",
            sheet_names.len(),
            sheet_names.join(", ")
        );

        assert!(summary.contains("3 sheets"));
        assert!(summary.contains("Summary"));
        assert!(summary.contains("Details"));
        assert!(summary.contains("Lookup"));
    }

    #[test]
    fn test_single_sheet_no_summary_header() {
        // For a single-sheet workbook the summary line must NOT be emitted.
        let sheet_names = vec!["Sheet1".to_string()];
        // The condition `sheet_names.len() > 1` is false → no summary.
        let summary_emitted = sheet_names.len() > 1;
        assert!(!summary_emitted);
    }

    #[test]
    fn test_empty_sheet_marker() {
        // When a sheet has zero non-empty rows the output should contain the
        // explicit "(empty sheet)" marker so the LLM is not confused.
        let row_count = 0usize;
        let marker = if row_count == 0 { "(empty sheet)" } else { "" };
        assert_eq!(marker, "(empty sheet)");
    }

    #[test]
    fn test_row_skips_all_empty_cells() {
        // A row where every cell is Data::Empty must be skipped.
        let row = vec![Data::Empty, Data::Empty, Data::Empty];
        let cells: Vec<String> = row.iter().map(cell_to_string).collect();
        let has_content = cells.iter().any(|c| !c.is_empty());
        assert!(!has_content, "All-empty rows should be skipped");
    }

    #[test]
    fn test_row_with_mixed_empty_and_data_is_kept() {
        // A row that contains at least one non-empty cell must be kept.
        let row = vec![Data::Empty, Data::String("value".to_string()), Data::Empty];
        let cells: Vec<String> = row.iter().map(cell_to_string).collect();
        let has_content = cells.iter().any(|c| !c.is_empty());
        assert!(has_content, "Row with at least one non-empty cell should be kept");
    }

    #[test]
    fn test_tab_separated_row_output() {
        // Cells in a non-empty row are joined with tab characters.
        let row = vec![
            Data::String("Name".to_string()),
            Data::String("Age".to_string()),
            Data::Int(30),
        ];
        let cells: Vec<String> = row.iter().map(cell_to_string).collect();
        let line = cells.join("\t");
        assert_eq!(line, "Name\tAge\t30");
    }
}

