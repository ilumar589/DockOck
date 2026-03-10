# Setup Guide

This guide walks you through everything needed to run DockOck from scratch.

---

## 1. Install Rust

DockOck requires Rust **1.75 or newer**.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable
```

Verify:

```bash
rustc --version   # e.g. rustc 1.80.0
cargo --version   # e.g. cargo 1.80.0
```

### System dependencies (Linux)

egui/eframe requires a few native libraries on Linux:

```bash
# Ubuntu / Debian
sudo apt-get install -y \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libssl-dev pkg-config

# Fedora / RHEL
sudo dnf install libxcb-devel libxkbcommon-devel openssl-devel
```

macOS and Windows do not need extra system dependencies.

---

## 2. Start the Ollama LLM Server

### Option A – Docker Compose (recommended)

Requires Docker Desktop or Docker Engine + Compose plugin.

```bash
docker-compose up -d
```

This:
1. Pulls `ollama/ollama:latest`
2. Starts the Ollama API on **http://localhost:11434**
3. Downloads the `llama3.2` model on first run (~2 GB)

Check that Ollama is healthy:

```bash
curl http://localhost:11434/api/tags
```

You should see JSON with a list of models.

To stop Ollama:

```bash
docker-compose down
```

### Option B – Ollama installed directly

1. Download from https://ollama.com/download
2. Start the server: `ollama serve`
3. Pull a model: `ollama pull llama3.2`

---

## 3. Build and Run DockOck

```bash
# Clone the repository (if you haven't already)
git clone https://github.com/ilumar589/DockOck.git
cd DockOck

# Build in release mode (recommended for performance)
cargo run --release
```

The first build downloads all crates from crates.io and may take 5–10 minutes.

Subsequent builds are incremental and fast.

---

## 4. Changing the Ollama Model

The default model is `llama3.2`. You can use any model supported by Ollama:

1. Pull the model: `ollama pull <model-name>`  
   Examples: `mistral`, `llama3.1`, `qwen2.5:7b`, `phi3`
2. In the DockOck UI, change the **Model** field in the top bar before clicking Generate.

### Recommended models for Gherkin generation

| Model | Size | Quality |
|-------|------|---------|
| `llama3.2` (default) | ~2 GB | Good |
| `llama3.1` | ~4 GB | Better |
| `qwen2.5:14b` | ~8 GB | Excellent |
| `mistral` | ~4 GB | Good |

---

## 5. Using Generated Gherkin with OpenSpec

1. In DockOck, after generating Gherkin, click **� Save All** to write all `.feature` files to your chosen output directory. Alternatively, click **📋 Copy** to copy individual files to the clipboard.
2. Open [OpenSpec](https://github.com/Fission-AI/OpenSpec) and import the `.feature` files.

---

## Troubleshooting

### "Cannot reach Ollama at http://localhost:11434"

- Ensure Docker Compose is running: `docker-compose ps`
- Or check that `ollama serve` is running in your terminal.
- On Linux, check firewall rules: `sudo ufw status`

### Build fails with missing system libraries

See the **System dependencies** section above for Linux packages.

### Model generates poor Gherkin

- Try a larger model (e.g. `llama3.1` or `qwen2.5:14b`).
- Ensure your source documents contain clear, descriptive text.
- Process related files together in one session to benefit from cross-file context.

### "Failed to parse word/document.xml"

- The file may be a legacy `.doc` (not `.docx`). Open it in Word and save as `.docx`.
- Encrypted/password-protected files are not supported.
