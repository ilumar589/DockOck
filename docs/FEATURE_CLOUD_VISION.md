# Feature Plan: Cloud Vision via AIArk `seed-2-0-lite-260228`

> Replace local Moondream (Ollama) with ByteDance AIArk's `seed-2-0-lite-260228` multimodal model for image description, providing richer context for Gherkin generation.

## Motivation

The current vision pipeline uses **Moondream** (~1.7B params) running locally on Ollama (port 11437). While it works, this has several limitations:

- **Quality**: Moondream's small size limits the quality of diagram/flowchart/UI descriptions
- **Infrastructure burden**: Requires a dedicated Docker container (`dockock-ollama-vision`) and local GPU/CPU resources
- **Inconsistency**: When using AIArk for Gen/Ext/Rev, vision remains the only local dependency — breaking full-cloud workflows

`seed-2-0-lite-260228` on AIArk supports **base64-encoded image input** via the OpenAI-compatible chat completions API, offers 262K context / 131K output, and is already configured in `custom_providers.json`.

## Current Architecture

```
File → Parse (Word/Excel/Visio)
  ├─ Extract text
  └─ Extract images → ExtractedImage { label, data: Vec<u8>, content_type }
       ↓
Vision Model (Moondream on Ollama port 11437)
  └─ POST /api/generate { model, prompt, images: [base64], stream: false }
  └─ OllamaGenerateResponse { response: String }
       ↓
Enriched text (raw text + "=== Embedded Image Descriptions ===")
       ↓
Generator/Extractor/Reviewer → Gherkin output
```

### Key code locations

| Component | File | Line(s) |
|-----------|------|---------|
| `DEFAULT_VISION_MODEL` constant | `src/llm/mod.rs` | ~52 |
| `VISION_DESCRIBE_PROMPT` | `src/llm/mod.rs` | ~248 |
| `AgentOrchestrator` struct (vision fields) | `src/llm/mod.rs` | ~299 |
| Vision endpoint probing (Ollama) | `src/llm/mod.rs` | ~370-397 |
| Vision endpoint probing (Custom) | `src/llm/mod.rs` | ~448-462 |
| `describe_image()` — raw HTTP to Ollama | `src/llm/mod.rs` | ~1417-1452 |
| `enrich_text_with_images()` | `src/llm/mod.rs` | ~1069-1135 |
| UI: "Vis:" model combo (always local) | `src/app.rs` | ~1069 |
| `custom_providers.json` — `seed-2-0-lite-260228` | `custom_providers.json` | model entry |

## Proposed Architecture

```
File → Parse (Word/Excel/Visio)
  ├─ Extract text
  └─ Extract images → ExtractedImage { label, data, content_type }
       ↓
  ┌──────────────────────────────────────────────┐
  │ Backend == Custom (AIArk)?                   │
  │   YES → OpenAI Chat Completions API          │
  │          POST /chat/completions              │
  │          model: "seed-2-0-lite-260228"       │
  │          messages: [{                         │
  │            role: "user",                      │
  │            content: [                         │
  │              { type: "text", text: prompt },  │
  │              { type: "image_url",             │
  │                image_url: {                   │
  │                  url: "data:{mime};base64,…"  │
  │                }                              │
  │              }                                │
  │            ]                                  │
  │          }]                                   │
  │   NO  → Ollama /api/generate (Moondream)     │
  │          (unchanged fallback)                 │
  └──────────────────────────────────────────────┘
       ↓
Enriched text → Generator → Gherkin
```

## Implementation Plan

### Phase 1: Provider config — add `vision` default model

**File: `custom_providers.json`**

Add a `"vision"` key to the `"defaults"` section:

```json
"defaults": {
  "generator": "deepseek-v3-2-251201",
  "extractor": "seed-1-6-flash-250715",
  "reviewer": "kimi-k2-thinking-251104",
  "vision": "seed-2-0-lite-260228"
}
```

**File: `src/llm/provider.rs`**

- Extend `CustomProviderConfig` (or the defaults parsing) to include an optional `vision` model ID.
- Expose it through `custom_model_ids()` or a dedicated `custom_vision_model()` accessor.

---

### Phase 2: Orchestrator — dual-path `describe_image()`

**File: `src/llm/mod.rs`**

#### 2a. Add a `vision_model` field to the custom defaults

When `backend == Custom`, populate the vision model from the provider config defaults instead of hardcoding `"moondream"`.

#### 2b. New method: `describe_image_openai()`

Create a new async method that calls the OpenAI-compatible chat completions endpoint with multimodal content:

```rust
async fn describe_image_openai(
    &self,
    image: &crate::parser::ExtractedImage,
) -> Result<String> {
    // 1. Check cache (same key logic as today)
    // 2. Build multimodal message:
    //    - content_type from image.content_type (e.g. "image/png")
    //    - base64 encode image.data
    //    - data URI: "data:{content_type};base64,{b64}"
    // 3. POST to {base_url}/chat/completions:
    //    {
    //      "model": self.vision_model,   // "seed-2-0-lite-260228"
    //      "messages": [{
    //        "role": "user",
    //        "content": [
    //          { "type": "text", "text": VISION_DESCRIBE_PROMPT },
    //          { "type": "image_url", "image_url": { "url": data_uri } }
    //        ]
    //      }],
    //      "max_tokens": 1024
    //    }
    // 4. Parse response.choices[0].message.content
    // 5. Store in cache
}
```

#### 2c. Route `describe_image()` based on backend

Modify the existing `describe_image()` to dispatch:

```rust
async fn describe_image(&self, image: &ExtractedImage) -> Result<String> {
    if self.openai_client.is_some() && !self.vision_endpoint_url.is_empty() {
        // Custom backend — use cloud vision
        self.describe_image_openai(image).await
    } else {
        // Ollama backend — existing Moondream path
        self.describe_image_ollama(image).await
    }
}
```

The current `describe_image()` body becomes `describe_image_ollama()` unchanged.

#### 2d. Store cloud vision base_url

The `openai_client` already has the base URL baked in, but for raw HTTP calls we need it explicitly. Options:
- **Option A**: Use the existing `openai_client` if rig-core's OpenAI client supports multimodal chat. This is ideal if the `rig` crate's `Prompt` trait can handle image content parts.
- **Option B** (recommended): Make a raw `reqwest` POST to `{base_url}/chat/completions` with the API key — mirrors the existing raw-HTTP pattern used for Ollama vision and avoids rig-core multimodal limitations.

Store the cloud vision URL and API key in the orchestrator:

```rust
pub struct AgentOrchestrator {
    // ... existing fields ...
    /// Cloud vision endpoint (when backend is Custom)
    cloud_vision_base_url: Option<String>,
    cloud_vision_api_key: Option<String>,
}
```

---

### Phase 3: Orchestrator constructor — wire up cloud vision

**File: `src/llm/mod.rs`** — `AgentOrchestrator::new()`

In the `ProviderBackend::Custom` match arm:

1. **Remove local-only requirement**: Currently, if the local vision Ollama endpoint is offline, `vision_endpoint_url` is set to empty string and image enrichment is silently skipped. With cloud vision, this should no longer gate functionality.

2. **Set cloud vision fields**:
```rust
ProviderBackend::Custom { name, base_url, api_key } => {
    // ... existing openai_client creation ...

    // Cloud vision available for free via the same API
    let cloud_vision_base_url = Some(base_url.clone());
    let cloud_vision_api_key = Some(api_key.clone());

    // Local Ollama vision is now optional fallback, not required
    let vis_ok = check_endpoint(&ENDPOINT_VISION).await;
    // ...
}
```

3. **Vision model selection**: Use the `"vision"` default from `custom_providers.json` if present, otherwise fall back to `"seed-2-0-lite-260228"`.

---

### Phase 4: UI — allow cloud vision model selection

**File: `src/app.rs`**

#### 4a. Vision model combo follows backend

Currently (line ~1069):
```rust
// Vision & Embedding always use local Ollama models
ui.label("Vis:");
model_combo(ui, "vis_model", &mut self.vision_model);
```

Change to:
```rust
ui.label("Vis:");
if self.backend.is_custom() {
    custom_model_combo(ui, "vis_model", &mut self.vision_model, &custom_models);
} else {
    model_combo(ui, "vis_model", &mut self.vision_model);
}
```

#### 4b. Default vision model when switching backends

When the user switches to an AIArk backend, set `self.vision_model` to the `"vision"` default from the provider config (e.g. `"seed-2-0-lite-260228"`). When switching back to Ollama, reset to `"moondream"`.

---

### Phase 5: Response parsing

**File: `src/llm/mod.rs`**

Add a response struct for the OpenAI chat completions format:

```rust
#[derive(serde::Deserialize)]
struct OpenAIChatResponse {
    choices: Vec<OpenAIChatChoice>,
}

#[derive(serde::Deserialize)]
struct OpenAIChatChoice {
    message: OpenAIChatMessage,
}

#[derive(serde::Deserialize)]
struct OpenAIChatMessage {
    content: String,
}
```

(These may already exist in rig-core — check before adding.)

---

### Phase 6: Cache key update

The vision cache key currently hashes `image.data + vision_model`. This naturally differentiates Moondream vs seed-2-0-lite results, so **no changes needed** — different model names already produce different cache keys.

---

### Phase 7: Testing & validation

1. **Unit test**: Mock the OpenAI chat completions endpoint, verify the multimodal payload format is correct (data URI, content parts array).
2. **Integration test**: Process a Visio file with embedded diagrams through the full pipeline using AIArk cloud vision. Compare Gherkin output quality vs Moondream.
3. **Fallback test**: Verify that when cloud API is unreachable, the system gracefully falls back to local Ollama vision (or reports a clear error).
4. **Cache test**: Verify that switching vision models invalidates the cache correctly (different model → different cache key).

---

## API Contract: AIArk Vision Request

```http
POST https://ark.ap-southeast.bytepluses.com/api/v3/chat/completions
Authorization: Bearer 03c63a6b-f2a0-40f1-bb21-3b3abea5c4ac
Content-Type: application/json

{
  "model": "seed-2-0-lite-260228",
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Describe this image in detail for a business analyst. Focus on: ..."
        },
        {
          "type": "image_url",
          "image_url": {
            "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg..."
          }
        }
      ]
    }
  ],
  "max_tokens": 1024
}
```

**Response:**
```json
{
  "choices": [
    {
      "message": {
        "role": "assistant",
        "content": "This image shows a flowchart depicting..."
      }
    }
  ]
}
```

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| AIArk rate limits / timeouts on large batches of images | Keep existing parallel-with-semaphore pattern; add retry with backoff |
| Image size exceeding API limits | Compress/resize images >4MB before base64 encoding |
| API cost per image | Cache aggressively (already implemented); show image count in UI before processing |
| Content type mismatch | Use `image.content_type` from parser; default to `image/png` if missing |
| rig-core incompatibility with multimodal | Use raw reqwest (Option B) — already proven pattern in codebase |

## Migration Path

- **No breaking changes**: Ollama+Moondream path remains the default for `ProviderBackend::Ollama`
- **Opt-in**: Cloud vision only activates when backend is `Custom` (AIArk)
- **Gradual**: Users can switch backend in UI dropdown — vision follows automatically
- **Reversible**: Switching back to Ollama reverts to Moondream instantly

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `custom_providers.json` | Add `"vision": "seed-2-0-lite-260228"` to defaults |
| `src/llm/provider.rs` | Parse `vision` from provider defaults |
| `src/llm/mod.rs` | Add `describe_image_openai()`, refactor `describe_image()` dispatch, add cloud vision fields to `AgentOrchestrator`, update constructor |
| `src/app.rs` | Make vision model combo follow backend selection, auto-default on backend switch |
