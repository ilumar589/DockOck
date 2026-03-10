# DockOck

> **Document → Gherkin** converter powered by a local [Ollama](https://ollama.com/) LLM and built with [egui](https://github.com/emilk/egui).

DockOck parses **Word** (`.docx`), **Excel** (`.xlsx`) and **Visio** (`.vsdx`) files and produces per-file [Gherkin](https://cucumber.io/docs/gherkin/) feature documentation that can be fed into [OpenSpec](https://github.com/Fission-AI/OpenSpec) to further generate context for project implementations.

---

## ✨ Features

| Feature | Details |
|---------|---------|
| Multi-file selection | Select as many files as you like in one session |
| Cross-file context | The LLM sees summaries of all previously processed files so references between documents are preserved |
| Word support | Extracts paragraph text from `.docx` archives |
| Excel support | Extracts cell data from every worksheet in `.xlsx` / `.xls` / `.ods` files |
| Visio support | Extracts shape labels and text from every page of `.vsdx` files |
| Local LLM | Runs 100 % locally via Ollama – no data leaves your machine |
| Configurable model | Any model supported by Ollama can be used (default: `llama3.2`) |
| One-click copy | Copy the generated `.feature` text to the clipboard |

---

## 🚀 Quick Start

### Prerequisites

| Tool | Install |
|------|---------|
| Rust (≥ 1.75) | [rustup.rs](https://rustup.rs) |
| Docker & Docker Compose | [docs.docker.com](https://docs.docker.com/get-started/get-docker/) |

### 1 – Start Ollama

```bash
docker-compose up -d
```

This pulls the `ollama/ollama` image, starts the server on port **11434**, and pulls the `llama3.2` model.  
Model data is persisted in the `ollama_data` Docker volume so subsequent starts are instant.

> **Without Docker** – if you have Ollama installed locally just run `ollama serve` in a separate terminal.

### 2 – Build and run DockOck

```bash
cargo run --release
```

The first build downloads all Rust crates and may take a few minutes.

### 3 – Use the app

1. Click **Check connection** to verify Ollama is reachable.
2. Click **➕ Add Files** and select one or more `.docx`, `.xlsx`, or `.vsdx` files.
3. Optionally change the **Model** name in the top bar (e.g. `mistral`, `llama3.1`).
4. Click **⚙ Generate Gherkin**.
5. Select any file in the left panel to view its generated `.feature` content.
6. Click **📋 Copy** to copy the Gherkin to your clipboard for use with OpenSpec.

---

## 📂 Project Structure

```
DockOck/
├── src/
│   ├── main.rs          – Entry point; bootstraps Tokio runtime + egui window
│   ├── app.rs           – egui application (state, UI, event loop)
│   ├── context.rs       – Shared cross-file context accumulator
│   ├── gherkin.rs       – Gherkin data structures + LLM output parser
│   ├── parser/
│   │   ├── mod.rs       – File-type dispatcher
│   │   ├── word.rs      – .docx parser (ZIP + XML)
│   │   ├── excel.rs     – .xlsx parser (calamine)
│   │   └── visio.rs     – .vsdx parser (ZIP + XML)
│   └── llm/
│       └── mod.rs       – Ollama integration via rig-core
├── Dockerfile.ollama    – Stand-alone Ollama Docker image
├── docker-compose.yml   – Recommended way to run Ollama locally
└── docs/
    ├── SETUP.md         – Detailed setup guide
    └── ARCHITECTURE.md  – Architecture and design decisions
```

---

## 📚 Further Reading

- [docs/SETUP.md](docs/SETUP.md) – Detailed environment setup and troubleshooting
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) – Architecture overview and design decisions
- [OpenSpec](https://github.com/Fission-AI/OpenSpec) – Use the generated Gherkin files here
- [rig-core](https://github.com/0xPlaygrounds/rig) – The Rust LLM framework used for Ollama integration

