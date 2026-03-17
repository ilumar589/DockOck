# Feature: Recursive Folder Import

## Overview

Allow the user to select an **entire folder** instead of individual files.
DockOck recursively walks every sub-directory, collects all files with
supported extensions, deduplicates against the current selection, and adds
them in one action.  This removes the tedium of manually picking dozens of
files spread across a folder tree.

**Supported extensions** (same as the existing file picker):
`.docx`, `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods`, `.vsdx`, `.vsd`, `.vsdm`

---

## User-Facing Behaviour

| Element | Detail |
|---------|--------|
| **New button** | "📁 Add Folder" in the left-panel toolbar, next to "➕ Add Files" |
| **Dialog** | Native folder picker via `rfd::FileDialog::pick_folder()` |
| **Recursion** | Walks the chosen directory recursively (all sub-folders) |
| **Filter** | Only files whose extension matches the supported set are added |
| **Hidden / system files** | Skipped (names starting with `.` or `~` — e.g. Word temp files `~$doc.docx`) |
| **Duplicates** | Files already in `selected_files` are silently skipped |
| **Status feedback** | One status line per added file, plus a summary: *"Added 14 files from …/folder"* |
| **Auto-grouping** | `recompute_groups()` is called after import, so auto-grouping applies as normal |
| **Disabled during processing** | Button is greyed out while a run is in progress (same as "Add Files") |

---

## Implementation Plan

### 1. Accepted-extensions constant (`src/parser/mod.rs`)

Extract the set of supported extensions into a shared constant so both the
file dialog filter and the folder walker use the same source of truth.

```rust
/// File extensions accepted by the parsers.
pub const ACCEPTED_EXTENSIONS: &[&str] = &[
    "docx", "xlsx", "xls", "xlsm", "xlsb", "ods", "vsdx", "vsd", "vsdm",
];
```

Update `open_file_dialog()` to reference `parser::ACCEPTED_EXTENSIONS` instead
of a hard-coded slice.

### 2. Folder-walk helper (`src/app.rs`)

Add a helper that collects matching files from a directory tree:

```rust
use std::path::{Path, PathBuf};

/// Recursively collect files with accepted extensions from `root`.
fn collect_supported_files(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let accepted: std::collections::HashSet<&str> =
        crate::parser::ACCEPTED_EXTENSIONS.iter().copied().collect();

    // Use walkdir or std::fs::read_dir recursive approach
    fn walk(dir: &Path, accepted: &std::collections::HashSet<&str>, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip hidden / temp files
            let name = path.file_name()
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
    results.sort();          // deterministic ordering
    results
}
```

> **Note:** Using `std::fs::read_dir` + manual recursion avoids adding a new
> dependency (`walkdir`).  If the project later adds `walkdir` for other
> purposes, this helper can be simplified.

### 3. Folder dialog method (`src/app.rs`)

```rust
/// Open a folder-picker dialog and recursively add all supported files.
fn open_folder_dialog(&mut self) {
    let folder = rfd::FileDialog::new().pick_folder();

    if let Some(folder) = folder {
        let found = collect_supported_files(&folder);
        let mut added = 0usize;
        for p in found {
            if !self.selected_files.contains(&p) {
                self.push_status(format!("Added: {}", p.display()));
                self.selected_files.push(p);
                added += 1;
            }
        }
        self.push_status(format!(
            "📁 Added {} file(s) from {}",
            added,
            folder.display()
        ));
        self.recompute_groups();
    }
}
```

### 4. UI button (`src/app.rs` — `render_left_panel`)

Insert the new button in the toolbar row, between "➕ Add Files" and "🗑 Clear":

```rust
ui.horizontal(|ui| {
    if ui
        .add_enabled(!is_processing, egui::Button::new("➕ Add Files"))
        .clicked()
    {
        self.open_file_dialog();
    }
    if ui
        .add_enabled(!is_processing, egui::Button::new("📁 Add Folder"))
        .clicked()
    {
        self.open_folder_dialog();
    }
    if ui
        .add_enabled(!is_processing, egui::Button::new("🗑 Clear"))
        .clicked()
    {
        self.clear_all();
    }
    // ... rest unchanged ...
});
```

### 5. Update `open_file_dialog` to use shared constant

```rust
fn open_file_dialog(&mut self) {
    let paths = rfd::FileDialog::new()
        .add_filter("Supported documents", crate::parser::ACCEPTED_EXTENSIONS)
        .pick_files();
    // ... rest unchanged ...
}
```

---

## File-Change Summary

| File | Change |
|------|--------|
| `src/parser/mod.rs` | Add `ACCEPTED_EXTENSIONS` constant |
| `src/app.rs` | Add `collect_supported_files()` helper |
| `src/app.rs` | Add `open_folder_dialog()` method |
| `src/app.rs` | Update `open_file_dialog()` to use `ACCEPTED_EXTENSIONS` |
| `src/app.rs` | Add "📁 Add Folder" button in `render_left_panel` toolbar |

---

## Edge Cases

| Case | Handling |
|------|----------|
| Empty folder (no matching files) | Status message: *"📁 Added 0 file(s) from …"* |
| Permission denied on sub-folder | Silently skipped (the `read_dir` call returns Err) |
| Symlink loops | `std::fs::read_dir` does not follow symlinks by default on Windows; safe |
| Very large folder tree (thousands of files) | Works — the walk is I/O bound, not CPU; the file dialog itself is the bottleneck |
| Folder already fully imported | All files are duplicates → 0 added, status message reflects this |
| Temp files (`~$*.docx`) | Filtered out by the `~` prefix check |

---

## No New Dependencies

The implementation uses only `std::fs` for directory walking and the existing
`rfd` crate for the folder picker dialog.  No new crates are required.
