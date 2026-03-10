# Architecture

This document describes the design decisions and component boundaries of DockOck.

---

## High-Level Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     DockOck Process                      │
│                                                          │
│  ┌──────────────┐   mpsc channel   ┌──────────────────┐ │
│  │  egui / UI   │◄─────────────────│ Processing Thread│ │
│  │  (main thd)  │                  │  (tokio runtime) │ │
│  └──────────────┘                  └──────────────────┘ │
│         │                                   │            │
│         │ rfd::FileDialog                   │            │
│         │                          ┌────────┴────────┐  │
│         ▼                          │  File Parsers   │  │
│  ┌──────────────┐                  │  word / excel   │  │
│  │ ProjectContext│                  │  / visio        │  │
│  │ (Arc<Mutex>) │                  └────────┬────────┘  │
│  └──────────────┘                           │            │
│                                    ┌────────▼────────┐  │
│                                    │   LLM Module    │  │
│                                    │  rig-core +     │  │
│                                    │  Ollama         │  │
│                                    └─────────────────┘  │
└─────────────────────────────────────────────────────────┘
                                              │
                              HTTP REST       │
                                              ▼
                              ┌───────────────────────────┐
                              │  Ollama Server             │
                              │  (Docker container or     │
                              │   local process)           │
                              │  http://localhost:11434    │
                              └───────────────────────────┘
```

---

## Module Breakdown

### `src/main.rs`

Entry point. Responsibilities:
- Initialise `tracing` for debug output.
- Create a `tokio::runtime::Runtime` and keep it alive in a background thread.
- Configure and launch the `eframe` native window.

### `src/app.rs`

The egui application (`DockOckApp`). Responsibilities:
- Own all UI state: file list, results map, status messages.
- Render the four UI regions: top bar, left panel (file list), right panel (Gherkin output), bottom bar.
- Open the file-picker dialog (`rfd::FileDialog`).
- Spawn the background processing task via `std::thread::spawn` and the Tokio handle.
- Poll the `mpsc::Receiver<ProcessingEvent>` every frame and apply incoming events.

### `src/context.rs`

`ProjectContext` – the shared context accumulator. Responsibilities:
- Store extracted `FileContent` for every file processed so far.
- Build a compact text summary (`build_summary()`) that is injected into the LLM prompt for every subsequent file, enabling cross-file awareness.

### `src/gherkin.rs`

Gherkin data structures. Responsibilities:
- `GherkinDocument` holds a parsed feature with scenarios and steps.
- `to_feature_string()` renders the struct as valid Gherkin syntax.
- `parse_from_llm_output()` parses the LLM's free-form text into the struct using a lightweight line-by-line parser.

### `src/parser/`

File parsers. Each parser extracts plain text from its format:

| Module | Format | Approach |
|--------|--------|---------|
| `word.rs` | `.docx` | Unzip → parse `word/document.xml` → collect `<w:t>` elements |
| `excel.rs` | `.xlsx` / `.xls` / `.ods` | `calamine` library; iterate worksheets + rows |
| `visio.rs` | `.vsdx` | Unzip → parse `visio/pages/pageN.xml` → collect `<Text>` and `<Cell N="Label">` elements |

`mod.rs` dispatches to the correct parser based on file extension.

### `src/llm/mod.rs`

LLM integration via `rig-core`. Responsibilities:
- Create an `ollama::Client` (connects to `http://localhost:11434`).
- Build an `Agent` with a fixed system preamble that instructs the model to output valid Gherkin.
- Construct the user prompt by combining the cross-file context summary with the extracted file text.
- Call `agent.prompt(...)` and return the raw response string.

---

## Threading Model

The egui event loop runs on the **main thread**. All I/O and LLM calls are performed on a **background thread** that drives the tokio runtime via `Handle::block_on`.

Events (status updates, file results) travel from the background thread to the UI thread through a `std::sync::mpsc` channel.  The UI polls this channel on every frame inside `poll_events()`.

```
Main thread                     Background thread
──────────────────────────────────────────────────
eframe::run_native()
  └─ App::update() each frame
       └─ poll_events()         ◄── mpsc::Receiver
                                       │
                                process_files()
                                  ├─ parse_file()  (blocking I/O)
                                  ├─ ctx.add_file()
                                  └─ generate_gherkin()  (async HTTP)
```

### Why `std::thread::spawn` + `Handle::block_on` instead of `tokio::spawn`?

eframe's `App::update` runs on the main thread and is a synchronous callback. The simplest way to drive async code from it is to spawn a regular OS thread and use `Handle::block_on` to run the async work on the existing Tokio runtime from that thread. This avoids the complexity of a custom async executor in the UI layer.

---

## Cross-File Context

When multiple files are processed in a single session, each file's extracted text is stored in `ProjectContext`. Before calling the LLM for file N, `context.build_summary()` serialises a short excerpt from every previously processed file and appends it to the prompt:

```
=== Cross-file project context ===

File: /path/to/design.docx
Type: Word
Excerpt: ... (first 400 chars) ...

File: /path/to/data-model.xlsx
Type: Excel
Excerpt: ... (first 400 chars) ...
```

This gives the LLM enough information to resolve cross-document references (e.g. an actor or entity defined in one file that is mentioned in another).

---

## Adding Support for New File Types

1. Create `src/parser/<format>.rs`.
2. Implement a `pub fn parse(path: &Path) -> Result<String>` function.
3. Register the new extension in `src/parser/mod.rs` inside `parse_file()`.
4. Add any new crate dependencies to `Cargo.toml`.

---

## OpenSpec Integration

The `.feature` files generated by DockOck follow the standard Gherkin grammar and can be imported directly into [OpenSpec](https://github.com/Fission-AI/OpenSpec). OpenSpec uses the feature files to generate boilerplate code, API contracts, or test skeletons depending on the configuration.

Suggested workflow:
1. Run DockOck on your specification documents (Word requirements, Excel data models, Visio process diagrams).
2. Copy each generated `.feature` file.
3. Feed the `.feature` files into OpenSpec to scaffold implementations.
