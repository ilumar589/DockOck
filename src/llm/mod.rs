//! Multi-agent LLM integration using `rig-core` with local Ollama instances.
//!
//! ## Architecture
//!
//! The pipeline has three configurable modes:
//!
//! | Mode     | Steps                                | LLM calls |
//! |----------|--------------------------------------|-----------|
//! | Fast     | Preprocess → Generate                | 1         |
//! | Standard | Preprocess → Generate → Review       | 2         |
//! | Full     | Extract(LLM) → Generate → Review     | 3         |
//!
//! The **Preprocess** step is a zero-cost Rust text truncation/structuring pass
//! that replaces the slow LLM extractor in Fast and Standard modes.
//!
//! Files are parsed in parallel, then processed through the agent pipeline
//! concurrently (up to `MAX_CONCURRENT` at a time).

use anyhow::{Context, Result};
use base64::prelude::*;
use futures::StreamExt;
use rig::agent::{MultiTurnStreamItem, Text};
use rig::client::{CompletionClient, Nothing};
use rig::completion::{Message, Prompt};
use rig::providers::ollama;
use rig::providers::openai;
use rig::streaming::{StreamedAssistantContent, StreamingPrompt};
use sha2::Digest;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::context::ProjectContext;

pub mod prefix_cache;
pub mod provider;
pub use prefix_cache::PrefixCache;
pub use provider::ProviderBackend;
pub use provider::{load_custom_providers, build_custom_backend, custom_model_ids, CustomProviderConfig};

/// Default model used for the generator agent.
pub const DEFAULT_GENERATOR_MODEL: &str = "qwen2.5-coder:32b";

/// Default model used for the extractor agent.
pub const DEFAULT_EXTRACTOR_MODEL: &str = "qwen2.5-coder:7b";

/// Default model used for the reviewer agent.
pub const DEFAULT_REVIEWER_MODEL: &str = "qwen2.5-coder:7b";

/// Default model used for describing images (must be a vision-capable model).
/// moondream is ~1.7B params and runs well on CPU-only setups.
pub const DEFAULT_VISION_MODEL: &str = "moondream";

/// Default maximum number of files processed through the LLM pipeline simultaneously.
pub const DEFAULT_MAX_CONCURRENT: usize = 20;

/// Maximum number of characters to send to the LLM in a single prompt.
/// Text beyond this limit is truncated with a note.
/// 12 000 chars ≈ 3 000 tokens — enough for a 14-page service document
/// while staying well within typical context windows.
const MAX_INPUT_CHARS: usize = 12_000;

/// Estimate a sensible input character budget based on the model name.
///
/// Larger-context models get a bigger budget so we can pass more of the
/// source document to the generator.  The returned value is in *characters*
/// (roughly 4 chars ≈ 1 token).  We leave headroom for the system prompt
/// and the generated output.
fn input_budget_for_model(model: &str) -> usize {
    let m = model.to_lowercase();

    // Models with 128k context
    if m.contains("128k") || m.contains("qwen2.5-coder:32b") || m.contains("qwen2.5:32b") {
        // ~25 000 tokens input → 100 000 chars
        100_000
    }
    // Models with 32k context
    else if m.contains("32k") || m.contains("deepseek") || m.contains("mixtral") {
        48_000
    }
    // Models with 8k context
    else if m.contains("7b") || m.contains("8b") || m.contains("llama3") {
        24_000
    }
    // Fallback — use the conservative default
    else {
        MAX_INPUT_CHARS
    }
}

/// Return the Ollama `num_ctx` value (in tokens) appropriate for the model.
///
/// Ollama defaults to 4096 which silently truncates long prompts.
/// We set this explicitly to match the model's true capability.
pub fn context_window_for_model(model: &str) -> u64 {
    let m = model.to_lowercase();

    if m.contains("qwen2.5-coder:32b") || m.contains("qwen2.5:32b") || m.contains("128k") {
        32_768
    } else if m.contains("deepseek") || m.contains("mixtral") || m.contains("32k")
        || m.contains("mistral-small")
    {
        32_768
    } else if m.contains("7b") || m.contains("8b") || m.contains("3b") || m.contains("mini") {
        16_384
    } else if m.contains("llama3") || m.contains("gemma") || m.contains("phi3") {
        16_384
    } else if m.contains("codellama") {
        16_384
    } else {
        // Safe default — 8k tokens, well above the 4096 Ollama default
        8_192
    }
}

/// Pipeline mode — controls which LLM stages are executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PipelineMode {
    /// Preprocess (fast) → Generate.  1 LLM call.
    Fast,
    /// Preprocess (fast) → Generate → Review.  2 LLM calls.
    Standard,
    /// Extract (LLM) → Generate → Review.  3 LLM calls.
    Full,
}

impl Default for PipelineMode {
    fn default() -> Self {
        Self::Fast
    }
}

impl std::fmt::Display for PipelineMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fast => write!(f, "Fast (1 LLM call)"),
            Self::Standard => write!(f, "Standard (2 LLM calls)"),
            Self::Full => write!(f, "Full (3 LLM calls)"),
        }
    }
}

impl PipelineMode {
    pub const ALL: [PipelineMode; 3] = [Self::Fast, Self::Standard, Self::Full];
}

/// Ollama instance definitions.
#[derive(Debug, Clone)]
pub struct OllamaEndpoint {
    pub name: &'static str,
    pub url: &'static str,
    pub port: u16,
}

pub const ENDPOINT_GENERATOR: OllamaEndpoint = OllamaEndpoint {
    name: "Generator",
    url: "http://localhost:11434",
    port: 11434,
};

pub const ENDPOINT_EXTRACTOR: OllamaEndpoint = OllamaEndpoint {
    name: "Extractor",
    url: "http://localhost:11435",
    port: 11435,
};

pub const ENDPOINT_REVIEWER: OllamaEndpoint = OllamaEndpoint {
    name: "Reviewer",
    url: "http://localhost:11436",
    port: 11436,
};

pub const ENDPOINT_VISION: OllamaEndpoint = OllamaEndpoint {
    name: "Vision",
    url: "http://localhost:11437",
    port: 11437,
};

// ─────────────────────────────────────────────
// Agent preambles
// ─────────────────────────────────────────────

const EXTRACTOR_PREAMBLE: &str = r#"You are an expert document analyst.
Your task is to read raw extracted document content and produce a concise structured summary.

Rules:
1. Identify the key actors, systems, data entities, and processes described.
2. List preconditions and postconditions for each process.
3. Capture business rules and validation logic.
4. Output in a structured format with sections: ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES,
   FIELD_SCOPING, LIFECYCLE_PHASES, SETUP_VS_RUNTIME, IMAGE_CONTENT.
5. Be concise — no more than 600 words.
6. Do not add conversational prose.
7. If the input contains an "=== Embedded Image Descriptions ===" section, you MUST include a
   dedicated IMAGE_CONTENT section in your summary that preserves:
   - XML/data structure hierarchies (element names, nesting, attributes) exactly as described
   - All diagram flows, decision points, and entity relationships
   - All reviewer/sidebar comments with author names and their full text
   - All cross-references to other documents
   Image-derived information is business-critical and must NOT be summarized away or omitted.
8. FIELD_SCOPING: For every entity that has a Create/New dialog, list ONLY the fields explicitly
   mentioned in that dialog section. Separately list fields that appear in FactBoxes, Consumers,
   or downstream documents. Never merge these two sets — they serve different purposes.
9. LIFECYCLE_PHASES: For every validation rule or business rule, tag it with the exact lifecycle
   phase it applies to (Creation, Edit, Category-change, Status-transition, Deletion). Only tag
   a rule as applying at Creation if the document explicitly states it applies during creation.
10. SETUP_VS_RUNTIME: Classify each entity or rule as either Setup/Configuration (e.g., category
    definitions, parameter tables, code lists) or Runtime/Business-object (e.g., premises,
    inspections, meters). Rules defined on a Setup entity must NOT be attributed to the
    corresponding Runtime entity unless the document explicitly says so.
11. DOCUMENT VERSION: The input text represents the FINAL accepted version of the document.
    Tracked changes, deleted text, and revision markup have already been stripped. If you see
    any residual revision artefacts (e.g., conflicting duplicate sentences, strikethrough
    markers, or inserted/deleted annotations), ignore them — treat only the final text as
    authoritative. Do not generate scenarios for obsolete or deleted requirements."#;

pub const GENERATOR_PREAMBLE: &str = r#"You are an expert business analyst and technical writer.
Your task is to read a structured document summary and produce well-structured Gherkin
Feature documentation that can be used by OpenSpec to generate project implementations.

Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Create meaningful Scenarios that cover the key behaviours described.
3. Use concrete, business-readable language in steps.
4. Where cross-file context is provided, reference other components or actors correctly.
5. Do not add explanatory prose outside the Gherkin block.
6. Always end with a blank line after the last Scenario.
7. If the input contains an "=== Embedded Image Descriptions ===" section, treat every
   image description as a first-class source of business requirements. Generate dedicated
   Scenarios for data structures (e.g. XML schemas), process flows, entity relationships,
   reviewer comments, and any business rules visible in those images. Do NOT ignore or
   skip image-derived content — it is equally important as the document text.
8. FIELD SCOPING — Creation scenarios must ONLY assert fields that are explicitly listed in the
   document's Create/New dialog section for that entity. Do NOT include fields from FactBoxes,
   Consumers, related entities, or downstream service documents in creation scenarios. If a field
   belongs to a FactBox or Consumer, place it in a separate viewing/navigation scenario instead.
9. SETUP vs RUNTIME — Clearly separate Setup/Configuration scenarios from Runtime/Business-object
   scenarios. If a rule (e.g., field immutability) is defined on a Setup entity (like a Category
   definition), do NOT apply it to the Runtime entity (like the business record that uses the
   category). Only assert runtime editability rules when the document ties them to a specific
   parameter or runtime condition.
10. LIFECYCLE PHASE ACCURACY — Each validation or business rule must be placed in a scenario that
    matches the exact lifecycle phase stated in the document (Creation, Edit, Category-change,
    Status-transition). Do NOT promote a validation to a creation scenario unless the document
    explicitly states it applies at creation time. Tag scenarios with the phase, e.g.:
    Scenario: [Creation] Manually create a new premises
    Scenario: [Category-change] Validate premises category change"#;

const REVIEWER_PREAMBLE: &str = r#"You are a Gherkin quality reviewer.
Your task is to review and improve a Gherkin Feature document.

Rules:
1. Fix any Gherkin syntax errors (Feature/Scenario/Given/When/Then/And/But).
2. Ensure scenarios are complete (have at least Given, When, Then).
3. Improve step clarity and business readability where needed.
4. Remove duplicate scenarios.
5. Output ONLY the corrected Gherkin — no explanations.
6. If the input is already good, return it unchanged.
7. FIELD SCOPING CHECK — If a creation scenario asserts fields that are typical of FactBoxes,
   Consumers, or related entities (e.g., hyperlinks to contracts, registration-level lookups,
   consumer lists), move those assertions to a separate viewing scenario or remove them from
   the creation scenario.
8. SETUP vs RUNTIME CHECK — If a scenario applies a Setup/Configuration rule (e.g., category
   name immutability) to a Runtime business object, correct it: either move the rule to a Setup
   scenario or rewrite it to reference the documented parameter that controls runtime behaviour.
9. LIFECYCLE PHASE CHECK — If a validation rule is asserted during creation but the context
   indicates it applies to a different phase (e.g., category change, status transition), move
   it to the correct lifecycle-phase scenario."#;

const GROUP_EXTRACTOR_PREAMBLE: &str = r#"You are an expert document analyst.
You will receive content extracted from MULTIPLE related documents that describe the same
system, feature, or process. Your task is to produce a single unified structured summary
that synthesises the information from all documents, resolving any overlaps or contradictions.

Rules:
1. Identify all actors, systems, data entities, and processes across ALL documents.
2. List preconditions and postconditions for each process.
3. Capture business rules and validation logic from every document.
4. Merge overlapping information — do not repeat the same fact from different documents.
5. Output in a structured format with sections: ACTORS, PROCESSES, BUSINESS_RULES, DATA_ENTITIES,
   FIELD_SCOPING, LIFECYCLE_PHASES, SETUP_VS_RUNTIME, IMAGE_CONTENT.
6. Be concise — no more than 900 words.
7. Do not add conversational prose.
8. If the input contains "=== Embedded Image Descriptions ===" sections, you MUST include a
   dedicated IMAGE_CONTENT section that preserves XML/data structure hierarchies, diagram flows,
   reviewer comments (with author names), and cross-references. Image content is business-critical.
9. FIELD_SCOPING: For every entity that has a Create/New dialog, list ONLY the fields explicitly
   mentioned in that dialog section. Separately list fields from FactBoxes, Consumers, or
   downstream documents. Never merge these two sets.
10. LIFECYCLE_PHASES: Tag every validation/business rule with its exact lifecycle phase
    (Creation, Edit, Category-change, Status-transition, Deletion) as stated in the source docs.
11. SETUP_VS_RUNTIME: Classify each entity or rule as Setup/Configuration or Runtime/Business-object.
    Rules from Setup entities must NOT be attributed to Runtime entities unless explicitly stated.
12. DOCUMENT VERSION: The input represents the FINAL accepted version of the documents.
    Tracked changes and revision markup have been stripped. If residual revision artefacts
    remain (duplicate sentences, strikethrough markers, inserted/deleted annotations), ignore
    them and treat only the final text as authoritative."#;

const GROUP_GENERATOR_PREAMBLE: &str = r#"You are an expert business analyst and technical writer.
You will receive a structured summary synthesised from MULTIPLE related documents that
describe the same system, feature, or process. Generate a single, cohesive Gherkin Feature
file that covers all scenarios described across the documents.

Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Create comprehensive Scenarios that cover behaviours from ALL source documents.
3. Avoid duplicate scenarios — merge overlapping processes into single scenarios.
4. Use concrete, business-readable language in steps.
5. Where cross-file context is provided, reference other components or actors correctly.
6. Do not add explanatory prose outside the Gherkin block.
7. Always end with a blank line after the last Scenario.
8. If the input contains an "=== Embedded Image Descriptions ===" section, treat every
   image description as a first-class source of business requirements. Generate dedicated
   Scenarios for data structures (e.g. XML schemas), process flows, entity relationships,
   reviewer comments, and any business rules visible in those images. Do NOT ignore or
   skip image-derived content — it is equally important as the document text.
9. FIELD SCOPING — Creation scenarios must ONLY assert fields explicitly listed in the Create/New
   dialog section. FactBox, Consumer, and downstream fields belong in separate viewing scenarios.
10. SETUP vs RUNTIME — Do not apply Setup/Configuration rules to Runtime business objects.
    Runtime editability must reference the documented parameter that controls it.
11. LIFECYCLE PHASE ACCURACY — Place each validation in the correct lifecycle-phase scenario.
    Tag scenarios with the phase, e.g.: Scenario: [Creation] ..., Scenario: [Category-change] ..."#;

const VISION_DESCRIBE_PROMPT: &str = "\
IMPORTANT: Every image in this document carries business-critical information that MUST be \
reflected in downstream Gherkin test scenarios. Your description must be detailed enough for \
another AI to generate complete, accurate Gherkin Feature files from it.
\
Describe this image in full detail for a business analyst. Focus on:
- Any text, labels, or annotations visible — transcribe them exactly
- Diagram type (flowchart, architecture, sequence, ER diagram, XML schema, etc.)
- Process flows and connections between elements — describe every path and decision point
- Tables, forms, or structured data — reproduce column headers and key data
- UI wireframes or screenshots — describe every field, button, and interaction
- XML or data structures: reproduce the element hierarchy verbatim (tag names, nesting, attributes)
- Sidebar comments, review notes, or annotations: transcribe each one with the author name and full text
- Info boxes, callouts, or warnings: reproduce their exact text content
- Section headings and numbering: preserve the document structure (e.g. 2.2.1, 2.2.2)
- Cross-references to other documents (e.g. 'Cf. D018 - LNA - CommonTypes')
- Business rules, constraints, or validation logic implied by the image
- Entity relationships and data dependencies
\
Capture ALL information thoroughly — nothing in the image is decorative. \
Every element represents a business rule, data structure, or process that must \
be testable. Do not summarize or omit structural details. Output plain text only.";

const MERGE_REVIEWER_PREAMBLE: &str = r#"You are a Gherkin merge specialist.
You will receive Gherkin output generated from multiple overlapping sections of the same document.
Your task is to merge them into a single cohesive Gherkin Feature.

Rules:
1. Output ONLY valid Gherkin syntax starting with "Feature:".
2. Combine all unique Scenarios — remove exact or near-duplicate scenarios.
3. If multiple chunks produced a Background, unify into one Background.
4. Preserve all unique business logic — do not drop scenarios.
5. Use consistent step wording and naming throughout.
6. Do not add explanatory prose outside the Gherkin block.
7. Always end with a blank line after the last Scenario."#;

/// Streaming chunk from Ollama's `/api/generate` endpoint.
#[derive(serde::Deserialize)]
struct OllamaStreamGenerateChunk {
    response: String,
    #[serde(default)]
    done: bool,
}

/// Streaming chunk from OpenAI-compatible `/chat/completions` endpoint (SSE).
#[derive(serde::Deserialize)]
struct OpenAIStreamChunk {
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(serde::Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIStreamDelta,
}

#[derive(serde::Deserialize)]
struct OpenAIStreamDelta {
    #[serde(default)]
    content: Option<String>,
}

// ─────────────────────────────────────────────
// Client creation
// ─────────────────────────────────────────────

fn create_client_for(endpoint: &OllamaEndpoint) -> Result<ollama::Client> {
    ollama::Client::builder()
        .api_key(Nothing)
        .base_url(endpoint.url)
        .build()
        .with_context(|| format!(
            "Failed to create Ollama client for {} at {}",
            endpoint.name, endpoint.url
        ))
}

/// The orchestrator that owns clients for all reachable LLM instances.
pub struct AgentOrchestrator {
    backend: ProviderBackend,
    /// Ollama clients (present when backend is Ollama)
    generator_client: Option<ollama::Client>,
    extractor_client: Option<ollama::Client>,
    reviewer_client: Option<ollama::Client>,
    /// OpenAI-compatible client (present when backend is Custom)
    openai_client: Option<openai::CompletionsClient>,
    vision_endpoint_url: String,
    /// Cloud vision base URL + API key (present when backend is Custom)
    cloud_vision_base_url: Option<String>,
    cloud_vision_api_key: Option<String>,
    generator_model: String,
    extractor_model: String,
    reviewer_model: String,
    vision_model: String,
    pub semaphore: Arc<Semaphore>,
    pub mode: PipelineMode,
    cache: crate::cache::DiskCache,
    /// KV-cache for generator shared prefix (Ollama only).
    generator_prefix_cache: Option<tokio::sync::Mutex<PrefixCache>>,
    /// Shared RAG indexes for `dynamic_context()` (chunks + memories).
    /// When non-empty, agents automatically inject relevant context per call.
    rag_indexes: Vec<crate::rag::SharedVectorIndex>,
}

/// Result of checking which Ollama endpoints are reachable.
#[derive(Debug, Clone)]
pub struct EndpointStatus {
    pub name: &'static str,
    pub url: &'static str,
    pub reachable: bool,
}

impl AgentOrchestrator {
    /// Create the orchestrator, probing endpoints as appropriate.
    /// For Ollama backend: at minimum the generator (port 11434) must be reachable.
    /// For Custom backend: only the vision endpoint is probed locally.
    pub async fn new(
        backend: ProviderBackend,
        generator_model: &str,
        extractor_model: &str,
        reviewer_model: &str,
        vision_model: &str,
        mode: PipelineMode,
        max_concurrent: usize,
        cache: crate::cache::DiskCache,
    ) -> Result<(Self, Vec<EndpointStatus>)> {
        let mut statuses = Vec::new();

        match &backend {
            ProviderBackend::Ollama => {
                // ── Ollama: probe all 4 local endpoints ──
                let gen_ok = check_endpoint(&ENDPOINT_GENERATOR).await;
                statuses.push(EndpointStatus {
                    name: ENDPOINT_GENERATOR.name,
                    url: ENDPOINT_GENERATOR.url,
                    reachable: gen_ok,
                });

                let ext_ok = check_endpoint(&ENDPOINT_EXTRACTOR).await;
                statuses.push(EndpointStatus {
                    name: ENDPOINT_EXTRACTOR.name,
                    url: ENDPOINT_EXTRACTOR.url,
                    reachable: ext_ok,
                });

                let rev_ok = check_endpoint(&ENDPOINT_REVIEWER).await;
                statuses.push(EndpointStatus {
                    name: ENDPOINT_REVIEWER.name,
                    url: ENDPOINT_REVIEWER.url,
                    reachable: rev_ok,
                });

                let vis_ok = check_endpoint(&ENDPOINT_VISION).await;
                statuses.push(EndpointStatus {
                    name: ENDPOINT_VISION.name,
                    url: ENDPOINT_VISION.url,
                    reachable: vis_ok,
                });

                if !gen_ok {
                    anyhow::bail!(
                        "Generator Ollama instance at {} is not reachable. \
                         At minimum this instance must be running.",
                        ENDPOINT_GENERATOR.url
                    );
                }

                let generator_client = Some(create_client_for(&ENDPOINT_GENERATOR)?);

                let extractor_client = if ext_ok {
                    info!("Extractor agent available at {}", ENDPOINT_EXTRACTOR.url);
                    Some(create_client_for(&ENDPOINT_EXTRACTOR)?)
                } else {
                    warn!("Extractor instance not available — generator will handle extraction");
                    None
                };

                let reviewer_client = if rev_ok {
                    info!("Reviewer agent available at {}", ENDPOINT_REVIEWER.url);
                    Some(create_client_for(&ENDPOINT_REVIEWER)?)
                } else {
                    warn!("Reviewer instance not available — skipping review step");
                    None
                };

                let vision_endpoint_url = if vis_ok {
                    info!("Vision agent available at {}", ENDPOINT_VISION.url);
                    ENDPOINT_VISION.url.to_string()
                } else {
                    warn!("Vision instance not available — falling back to extractor/generator for vision");
                    if ext_ok {
                        ENDPOINT_EXTRACTOR.url.to_string()
                    } else {
                        ENDPOINT_GENERATOR.url.to_string()
                    }
                };

                let active_count = 1 + ext_ok as usize + rev_ok as usize + vis_ok as usize;
                let concurrency = max_concurrent.max(active_count);

                Ok((
                    Self {
                        backend,
                        generator_client,
                        extractor_client,
                        reviewer_client,
                        openai_client: None,
                        vision_endpoint_url,
                        cloud_vision_base_url: None,
                        cloud_vision_api_key: None,
                        generator_model: generator_model.to_string(),
                        extractor_model: extractor_model.to_string(),
                        reviewer_model: reviewer_model.to_string(),
                        vision_model: vision_model.to_string(),
                        semaphore: Arc::new(Semaphore::new(concurrency)),
                        mode,
                        cache,
                        generator_prefix_cache: Some(tokio::sync::Mutex::new(
                            PrefixCache::new(ENDPOINT_GENERATOR.url, generator_model)
                        )),
                        rag_indexes: Vec::new(),
                    },
                    statuses,
                ))
            }
            ProviderBackend::Custom { name, base_url, api_key } => {
                // ── Custom: single OpenAI-compatible client for text roles ──
                info!("Using custom provider: {name} at {base_url}");

                // Build a reqwest::Client with explicit timeouts so cloud API
                // requests don't hang indefinitely on DNS/connect stalls.
                // Note: we do NOT set an overall `timeout()` because SSE
                // streaming responses can legitimately run for many minutes.
                // Per-chunk stall detection is handled in stream_chat_with_progress.
                let http_client = reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .read_timeout(std::time::Duration::from_secs(90))
                    .build()
                    .unwrap_or_default();

                let openai_client = openai::CompletionsClient::builder()
                    .api_key(api_key)
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .with_context(|| format!("Failed to create OpenAI-compatible client for {}", name))?;

                // Cloud vision is available via the same API
                let cloud_vision_base_url = Some(base_url.clone());
                let cloud_vision_api_key = Some(api_key.clone());

                // Probe local vision endpoint as optional fallback
                let vis_ok = check_endpoint(&ENDPOINT_VISION).await;
                statuses.push(EndpointStatus {
                    name: "Cloud API",
                    url: Box::leak(base_url.clone().into_boxed_str()),
                    reachable: true, // assume cloud is reachable; errors surface at call time
                });
                if vis_ok {
                    statuses.push(EndpointStatus {
                        name: ENDPOINT_VISION.name,
                        url: ENDPOINT_VISION.url,
                        reachable: true,
                    });
                }

                // Local Ollama vision URL — still used if model is a local Ollama model
                let vision_endpoint_url = if vis_ok {
                    info!("Local vision agent available at {}", ENDPOINT_VISION.url);
                    ENDPOINT_VISION.url.to_string()
                } else {
                    info!("Local vision not available — using cloud vision via {name}");
                    String::new()
                };

                Ok((
                    Self {
                        backend,
                        generator_client: None,
                        extractor_client: None,
                        reviewer_client: None,
                        openai_client: Some(openai_client),
                        vision_endpoint_url,
                        cloud_vision_base_url,
                        cloud_vision_api_key,
                        generator_model: generator_model.to_string(),
                        extractor_model: extractor_model.to_string(),
                        reviewer_model: reviewer_model.to_string(),
                        vision_model: vision_model.to_string(),
                        semaphore: Arc::new(Semaphore::new(max_concurrent)),
                        mode,
                        cache,
                        generator_prefix_cache: None, // not applicable for cloud APIs
                        rag_indexes: Vec::new(),
                    },
                    statuses,
                ))
            }
        }
    }

    /// Set the RAG vector store indexes for `dynamic_context()` integration.
    /// When set, agents will automatically retrieve relevant cross-file context
    /// from MongoDB vector indexes on each LLM call.
    pub fn set_rag_indexes(&mut self, indexes: Vec<crate::rag::SharedVectorIndex>) {
        self.rag_indexes = indexes;
    }

    /// Prime the generator's KV-cache prefix (Ollama only).
    pub async fn prime_generator_prefix(&self, preamble: &str, glossary: &str) -> Result<()> {
        if glossary.is_empty() {
            return Ok(());
        }
        let Some(ref cache_mutex) = self.generator_prefix_cache else {
            return Ok(()); // custom backend — no prefix cache
        };
        let num_ctx = context_window_for_model(&self.generator_model);
        let mut cache = cache_mutex.lock().await;
        cache.prime(preamble, glossary, num_ctx).await
    }

    /// Whether the generator prefix cache is primed and ready.
    #[allow(dead_code)]
    pub async fn has_generator_prefix_cache(&self, preamble: &str, glossary: &str) -> bool {
        let Some(ref cache_mutex) = self.generator_prefix_cache else {
            return false;
        };
        let cache = cache_mutex.lock().await;
        cache.is_primed_for(preamble, glossary)
    }

    /// Send a trivial prompt to each reachable endpoint to force model loading.
    /// Skipped for custom (cloud) backends.
    pub async fn warm_up(&self) {
        if self.backend.is_custom() {
            return; // hosted APIs have no cold start
        }

        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // Warm up generator (always present for Ollama)
        if let Some(client) = &self.generator_client {
            let client = client.clone();
            let model = self.generator_model.clone();
            handles.push(tokio::spawn(async move {
                let agent = client.agent(&model).build();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    agent.prompt("Hi"),
                ).await;
            }));
        }

        // Warm up extractor if available
        if let Some(client) = &self.extractor_client {
            let client = client.clone();
            let model = self.extractor_model.clone();
            handles.push(tokio::spawn(async move {
                let agent = client.agent(&model).build();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    agent.prompt("Hi"),
                ).await;
            }));
        }

        // Warm up reviewer if available
        if let Some(client) = &self.reviewer_client {
            let client = client.clone();
            let model = self.reviewer_model.clone();
            handles.push(tokio::spawn(async move {
                let agent = client.agent(&model).build();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    agent.prompt("Hi"),
                ).await;
            }));
        }

        // Warm up vision via raw HTTP (always local)
        if !self.vision_endpoint_url.is_empty() {
            let url = self.vision_endpoint_url.clone();
            let model = self.vision_model.clone();
            handles.push(tokio::spawn(async move {
                let client = reqwest::Client::new();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    client.post(format!("{}/api/generate", url))
                        .json(&serde_json::json!({
                            "model": model,
                            "prompt": "Hi",
                            "stream": false
                        }))
                        .send(),
                ).await;
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Whether this orchestrator uses a custom (non-Ollama) backend.
    pub fn is_custom_backend(&self) -> bool {
        self.backend.is_custom()
    }

    // ── Internal helper: dispatch chat to the right backend ──

    /// Check whether an error message indicates a retryable transient failure
    /// (rate-limit, server overload, connection issue).
    fn is_retryable_error(err_msg: &str) -> bool {
        let lower = err_msg.to_lowercase();
        lower.contains("429")
            || lower.contains("too many requests")
            || lower.contains("rate limit")
            || lower.contains("ratelimit")
            || lower.contains("tpm")
            || lower.contains("rpm")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("529")
            || lower.contains("bad gateway")
            || lower.contains("service unavailable")
            || lower.contains("overloaded")
            || lower.contains("connection reset")
            || lower.contains("connection refused")
            || lower.contains("broken pipe")
    }

    /// Retry delays for transient errors (exponential backoff).
    const RETRY_DELAYS: [std::time::Duration; 3] = [
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(15),
        std::time::Duration::from_secs(30),
    ];

    /// Build an agent for one of the Ollama text roles and stream a chat.
    /// `ollama_client` is the Ollama client for this role (may fall back to generator).
    #[tracing::instrument(
        name = "llm.run_ollama_chat",
        skip(ollama_client, preamble, prompt, history, rag_indexes, status_tx, cancel_token),
        fields(model, stage_name, file_name)
    )]
    async fn run_ollama_chat(
        ollama_client: &ollama::Client,
        model: &str,
        preamble: &str,
        prompt: &str,
        history: Vec<Message>,
        stage_name: &str,
        file_name: &str,
        rag_indexes: &[crate::rag::SharedVectorIndex],
        status_tx: &std::sync::mpsc::Sender<String>,
        timeout: std::time::Duration,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let num_ctx = context_window_for_model(model);
        let mut last_err = None;
        for attempt in 0..=Self::RETRY_DELAYS.len() {
            if cancel_token.is_cancelled() {
                anyhow::bail!("{stage_name} cancelled for {file_name}");
            }
            let mut builder = ollama_client
                .agent(model)
                .preamble(preamble)
                .additional_params(serde_json::json!({"num_ctx": num_ctx}));
            for idx in rag_indexes {
                builder = builder.dynamic_context(8, idx.clone());
            }
            let agent = builder.build();
            match stream_chat_with_progress(&agent, prompt, history.clone(), stage_name, file_name, status_tx, timeout, cancel_token).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let msg = format!("{e}");
                    if attempt < Self::RETRY_DELAYS.len() && Self::is_retryable_error(&msg) {
                        let delay = Self::RETRY_DELAYS[attempt];
                        let _ = status_tx.send(format!(
                            "⏳ [{stage_name}] {file_name}: rate-limited, retrying in {}s (attempt {}/{})…",
                            delay.as_secs(), attempt + 1, Self::RETRY_DELAYS.len()
                        ));
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {},
                            _ = cancel_token.cancelled() => {
                                anyhow::bail!("{stage_name} cancelled for {file_name} during retry backoff");
                            }
                        }
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap())
    }

    /// Build an agent for the OpenAI-compatible backend and stream a chat.
    #[tracing::instrument(
        name = "llm.run_openai_chat",
        skip(openai_client, preamble, prompt, history, rag_indexes, status_tx, cancel_token),
        fields(model, stage_name, file_name)
    )]
    async fn run_openai_chat(
        openai_client: &openai::CompletionsClient,
        model: &str,
        preamble: &str,
        prompt: &str,
        history: Vec<Message>,
        stage_name: &str,
        file_name: &str,
        rag_indexes: &[crate::rag::SharedVectorIndex],
        status_tx: &std::sync::mpsc::Sender<String>,
        timeout: std::time::Duration,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let mut last_err = None;
        for attempt in 0..=Self::RETRY_DELAYS.len() {
            if cancel_token.is_cancelled() {
                anyhow::bail!("{stage_name} cancelled for {file_name}");
            }
            let mut builder = openai_client
                .agent(model)
                .preamble(preamble);
            for idx in rag_indexes {
                builder = builder.dynamic_context(8, idx.clone());
            }
            let agent = builder.build();
            match stream_chat_with_progress(&agent, prompt, history.clone(), stage_name, file_name, status_tx, timeout, cancel_token).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let msg = format!("{e}");
                    if attempt < Self::RETRY_DELAYS.len() && Self::is_retryable_error(&msg) {
                        let delay = Self::RETRY_DELAYS[attempt];
                        let _ = status_tx.send(format!(
                            "⏳ [{stage_name}] {file_name}: rate-limited, retrying in {}s (attempt {}/{})…",
                            delay.as_secs(), attempt + 1, Self::RETRY_DELAYS.len()
                        ));
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {},
                            _ = cancel_token.cancelled() => {
                                anyhow::bail!("{stage_name} cancelled for {file_name} during retry backoff");
                            }
                        }
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap())
    }

    /// Resolve the Ollama client for the extractor role (falls back to generator).
    fn ollama_extractor_client(&self) -> &ollama::Client {
        self.extractor_client
            .as_ref()
            .or(self.generator_client.as_ref())
            .expect("at least generator client must exist for Ollama backend")
    }

    /// Resolve the model name for the extractor role.
    fn effective_extractor_model(&self) -> &str {
        if self.extractor_client.is_some() {
            &self.extractor_model
        } else if self.backend.is_custom() {
            &self.extractor_model
        } else {
            &self.generator_model
        }
    }

    /// Resolve the Ollama client for the reviewer role (falls back to generator).
    fn ollama_reviewer_client(&self) -> &ollama::Client {
        self.reviewer_client
            .as_ref()
            .or(self.generator_client.as_ref())
            .expect("at least generator client must exist for Ollama backend")
    }

    /// Resolve the model name for the reviewer role.
    fn effective_reviewer_model(&self) -> &str {
        if self.reviewer_client.is_some() {
            &self.reviewer_model
        } else if self.backend.is_custom() {
            &self.reviewer_model
        } else {
            &self.generator_model
        }
    }

    /// Run the pipeline for one file. Stages depend on `self.mode`.
    /// Results are cached by content hash when `force_regenerate` is false.
    /// When RAG dynamic_context indexes are configured, cross-file context is
    /// injected automatically by rig-core; otherwise the local ProjectContext
    /// excerpt is used.
    #[tracing::instrument(
        name = "llm.process_file",
        skip(self, raw_text, images, context, status_tx, cancel_token),
        fields(file_name, file_type, pipeline_mode = ?self.mode)
    )]
    pub async fn process_file(
        &self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
        images: &[crate::parser::ExtractedImage],
        context: &ProjectContext,
        status_tx: &std::sync::mpsc::Sender<String>,
        force_regenerate: bool,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Build LLM cache key from all inputs that affect the output.
        // When RAG indexes are active, dynamic context is retrieved per-call
        // and we leave context_summary empty (cache key changes only when
        // the document itself changes).
        let context_summary = if self.rag_indexes.is_empty() {
            context.build_summary()
        } else {
            String::new()
        };
        let images_hash = {
            let mut h = sha2::Sha256::new();
            for img in images {
                sha2::Digest::update(&mut h, &img.data);
            }
            format!("{:x}", h.finalize())
        };
        let llm_cache_key = crate::cache::composite_key(&[
            file_name.as_bytes(),
            raw_text.as_bytes(),
            format!("{:?}", self.mode).as_bytes(),
            self.generator_model.as_bytes(),
            self.extractor_model.as_bytes(),
            self.reviewer_model.as_bytes(),
            images_hash.as_bytes(),
            context_summary.as_bytes(),
        ]);

        // Check LLM cache
        if !force_regenerate {
            if let Some(cached) = self.cache.get_text(crate::cache::NS_LLM, &llm_cache_key) {
                let _ = status_tx.send(format!(
                    "📦 [Cache] {} — loaded from cache", file_name
                ));
                return Ok(cached);
            }
        }

        // ── Step 0: Describe images with vision model ──
        let enriched_text = if !images.is_empty() {
            let _ = status_tx.send(format!(
                "👁 [Vision] Describing {} image(s) from {}…", images.len(), file_name
            ));
            self.enrich_text_with_images(raw_text, images, file_name, status_tx, cancel_token).await
        } else {
            raw_text.to_string()
        };

        // Determine which model drives the input budget (extractor in Full, generator otherwise)
        let budget_model = if self.mode == PipelineMode::Full {
            if self.extractor_client.is_some() || self.openai_client.is_some() { &self.extractor_model } else { &self.generator_model }
        } else {
            &self.generator_model
        };

        // Pre-compute cross-file context overhead so chunking accounts for it
        let glossary = context.build_glossary();
        let context_overhead = context_summary.len() + glossary.len();

        // ── Chunk-and-merge path for oversized documents ──
        if needs_chunking(&enriched_text, budget_model, context_overhead) {
            let result = self.process_file_chunked(
                file_name, file_type, &enriched_text, context, context_overhead, status_tx, cancel_token,
            ).await?;
            self.cache.put_text(crate::cache::NS_LLM, &llm_cache_key, &result);
            return Ok(result);
        }

        // ── Step 1: Prepare input for the generator ──
        let budget = input_budget_for_model(&self.generator_model);
        let summary = if self.mode == PipelineMode::Full {
            // Full mode: use LLM extractor
            let _ = status_tx.send(format!(
                "🔍 [Extractor] Analysing {}…", file_name
            ));
            self.extract(file_name, file_type, &enriched_text, status_tx, cancel_token).await
                .unwrap_or_else(|e| {
                    warn!("Extraction failed for {}: {} — falling back to preprocessor", file_name, e);
                    preprocess_text(&enriched_text, file_name, file_type, budget)
                })
        } else {
            // Fast / Standard: instant Rust preprocessor (no LLM)
            let _ = status_tx.send(format!(
                "⚡ [Preprocess] Structuring {}…", file_name
            ));
            preprocess_text(&enriched_text, file_name, file_type, budget)
        };

        // ── Step 2: Generate Gherkin ──
        let _ = status_tx.send(format!(
            "⚙ [Generator] Creating Gherkin for {}…", file_name
        ));

        let gherkin = self.generate(file_name, &summary, &context_summary, &glossary, status_tx, cancel_token).await?;

        // ── Step 3: Review / refine (Standard and Full modes only) ──
        let do_review = self.mode != PipelineMode::Fast
            && (self.reviewer_client.is_some() || self.openai_client.is_some());
        let result = if do_review {
            let _ = status_tx.send(format!(
                "✅ [Reviewer] Validating Gherkin for {}…", file_name
            ));
            match self.review(file_name, &gherkin, status_tx, cancel_token).await {
                Ok(refined) => refined,
                Err(e) => {
                    warn!("Review failed for {}: {} — using unreviewed output", file_name, e);
                    gherkin
                }
            }
        } else {
            gherkin
        };

        // Store in LLM cache
        self.cache.put_text(crate::cache::NS_LLM, &llm_cache_key, &result);

        Ok(result)
    }

    /// Chunked pipeline for documents that exceed the model's context window.
    /// Splits text into overlapping windows, processes each chunk through
    /// extract/preprocess → generate, then merges all Gherkin via a merge-review pass.
    #[tracing::instrument(
        name = "llm.process_file_chunked",
        skip(self, enriched_text, context, status_tx, cancel_token),
        fields(file_name, file_type)
    )]
    async fn process_file_chunked(
        &self,
        file_name: &str,
        file_type: &str,
        enriched_text: &str,
        context: &ProjectContext,
        context_overhead: usize,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let budget_model = if self.mode == PipelineMode::Full {
            if self.extractor_client.is_some() || self.openai_client.is_some() { &self.extractor_model } else { &self.generator_model }
        } else {
            &self.generator_model
        };
        let chunks = chunk_for_llm(enriched_text, budget_model, context_overhead);
        let n = chunks.len();

        let _ = status_tx.send(format!(
            "📐 [Chunked] {}: splitting into {} chunks (exceeds context window)",
            file_name, n
        ));

        // Phase 1: Extract/preprocess each chunk (can run concurrently)
        let budget = input_budget_for_model(&self.generator_model);
        let mut summaries: Vec<String> = Vec::with_capacity(n);

        for chunk in &chunks {
            if cancel_token.is_cancelled() {
                anyhow::bail!("Cancelled during chunked extraction for {file_name}");
            }
            let chunk_label = format!("{} [{}/{}]", file_name, chunk.index + 1, chunk.total);

            let summary = if self.mode == PipelineMode::Full {
                let _ = status_tx.send(format!(
                    "🔍 [Extractor] Analysing {}…", chunk_label
                ));
                self.extract(&chunk_label, file_type, &chunk.text, status_tx, cancel_token)
                    .await
                    .unwrap_or_else(|e| {
                        warn!("Extraction failed for {}: {} — falling back to preprocessor", chunk_label, e);
                        preprocess_text(&chunk.text, &chunk_label, file_type, budget)
                    })
            } else {
                let _ = status_tx.send(format!(
                    "⚡ [Preprocess] Structuring {}…", chunk_label
                ));
                preprocess_text(&chunk.text, &chunk_label, file_type, budget)
            };
            summaries.push(summary);
        }

        // Phase 2: Generate Gherkin for each chunk, with prior summaries as context hints
        let glossary = context.build_glossary();
        let context_summary = if self.rag_indexes.is_empty() {
            context.build_summary()
        } else {
            String::new()
        };
        let mut chunk_gherkins: Vec<String> = Vec::with_capacity(n);

        for (i, summary) in summaries.iter().enumerate() {
            if cancel_token.is_cancelled() {
                anyhow::bail!("Cancelled during chunked generation for {file_name}");
            }
            let chunk_label = format!("{} [{}/{}]", file_name, i + 1, n);
            let _ = status_tx.send(format!(
                "⚙ [Generator] Creating Gherkin for {}…", chunk_label
            ));

            // Build a context hint from other chunks' summaries
            let mut other_summaries = String::new();
            for (j, s) in summaries.iter().enumerate() {
                if j != i {
                    other_summaries.push_str(&format!(
                        "--- Summary from part {}/{} ---\n{}\n\n",
                        j + 1, n, &s[..s.len().min(500)]
                    ));
                }
            }
            let chunk_context = if other_summaries.is_empty() {
                context_summary.clone()
            } else {
                format!(
                    "{}\n\n=== Summaries from other parts of the same document ===\n{}",
                    context_summary, other_summaries
                )
            };

            let gherkin = self.generate(
                &chunk_label, summary, &chunk_context, &glossary, status_tx, cancel_token,
            ).await?;
            chunk_gherkins.push(gherkin);
        }

        // Phase 3: Merge all chunk Gherkin via merge-reviewer
        self.merge_chunk_gherkin(file_name, &chunk_gherkins, status_tx, cancel_token).await
    }

    /// Merge Gherkin from multiple chunks of the same document into one cohesive Feature.
    async fn merge_chunk_gherkin(
        &self,
        file_name: &str,
        chunk_gherkins: &[String],
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // If only one chunk, no merge needed
        if chunk_gherkins.len() == 1 {
            return Ok(chunk_gherkins[0].clone());
        }

        let _ = status_tx.send(format!(
            "🔀 [Merge] {}: merging {} chunks into single Feature…",
            file_name,
            chunk_gherkins.len()
        ));

        let mut combined = String::new();
        for (i, g) in chunk_gherkins.iter().enumerate() {
            combined.push_str(&format!(
                "=== Gherkin from Part {}/{} ===\n{}\n\n",
                i + 1,
                chunk_gherkins.len(),
                g
            ));
        }

        // Use the generator model for the merge (it's the most capable)
        let history = vec![
            Message::user(combined),
        ];
        let prompt = format!(
            "Merge the {} Gherkin chunks above into a single cohesive Feature for '{}'.",
            chunk_gherkins.len(),
            file_name
        );

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, &self.generator_model, MERGE_REVIEWER_PREAMBLE,
                &prompt, history, "Merge", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.generator_client.as_ref().expect("Ollama generator required"),
                &self.generator_model, MERGE_REVIEWER_PREAMBLE,
                &prompt, history, "Merge", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        }
    }

    #[tracing::instrument(
        name = "llm.extract",
        skip(self, raw_text, status_tx, cancel_token),
        fields(file_name, file_type)
    )]
    async fn extract(
        &self,
        file_name: &str,
        file_type: &str,
        raw_text: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let model = self.effective_extractor_model();
        let history = vec![
            Message::user(format!(
                "Document metadata:\nFile: {file_name}\nType: {file_type}"
            )),
            Message::user(format!(
                "=== Document Content ===\n{raw_text}"
            )),
        ];

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, model, EXTRACTOR_PREAMBLE,
                "Produce the structured summary now.",
                history, "Extractor", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(120),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.ollama_extractor_client(), model, EXTRACTOR_PREAMBLE,
                "Produce the structured summary now.",
                history, "Extractor", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(120),
                cancel_token,
            ).await
        }
    }

    #[tracing::instrument(
        name = "llm.generate",
        skip(self, summary, context_summary, glossary, status_tx, cancel_token),
        fields(file_name, context_len = context_summary.len())
    )]
    async fn generate(
        &self,
        file_name: &str,
        summary: &str,
        context_summary: &str,
        glossary: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Try prefix-cached path first (Ollama only — skips recomputing shared prefix attention).
        // When RAG dynamic_context is active, skip prefix cache because it
        // bypasses agent construction and cannot inject retrieved chunks.
        if self.rag_indexes.is_empty() {
            if let Some(ref cache_mutex) = self.generator_prefix_cache {
                let num_ctx = context_window_for_model(&self.generator_model);
                let cache = cache_mutex.lock().await;
                if cache.is_primed_for(GENERATOR_PREAMBLE, glossary) {
                    // Build per-file suffix only (glossary is in the cached prefix)
                    let mut suffix = String::new();
                    if !context_summary.contains("No prior files") && !context_summary.is_empty() {
                        suffix.push_str(context_summary);
                        suffix.push('\n');
                    }
                    suffix.push_str(&format!(
                        "=== Structured Summary ===\n{summary}\n\n\
                         Generate the Gherkin Feature for document: {file_name}"
                    ));

                    return cache.stream_generate(
                        &suffix,
                        num_ctx,
                        "Generator",
                        file_name,
                        status_tx,
                        std::time::Duration::from_secs(180),
                    ).await;
                }
            }
        }

        // Fallback: multi-turn chat via appropriate backend
        let mut history: Vec<Message> = Vec::new();

        if !glossary.is_empty() {
            history.push(Message::user(glossary.to_owned()));
        }

        if !context_summary.contains("No prior files") && !context_summary.is_empty() {
            history.push(Message::user(context_summary.to_owned()));
        }

        // When RAG indexes are active, fold the structured summary INTO the
        // prompt text so that rig-core's `rag_text()` (which reads the prompt
        // message) captures meaningful content for vector retrieval.  Without
        // RAG the summary stays in a preceding chat-history message.
        let prompt = if !self.rag_indexes.is_empty() {
            format!(
                "=== Structured Summary ===\n{summary}\n\n\
                 Generate the Gherkin Feature for document: {file_name}"
            )
        } else {
            history.push(Message::user(format!(
                "=== Structured Summary ===\n{summary}"
            )));
            format!("Generate the Gherkin Feature for document: {file_name}")
        };

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, &self.generator_model, GENERATOR_PREAMBLE,
                &prompt, history, "Generator", file_name, &self.rag_indexes,
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.generator_client.as_ref().expect("Ollama generator required"),
                &self.generator_model, GENERATOR_PREAMBLE,
                &prompt, history, "Generator", file_name, &self.rag_indexes,
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        }
    }

    #[tracing::instrument(
        name = "llm.review",
        skip(self, gherkin, status_tx, cancel_token),
        fields(file_name)
    )]
    async fn review(
        &self,
        file_name: &str,
        gherkin: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let model = self.effective_reviewer_model();
        let history = vec![
            Message::user(gherkin.to_owned()),
        ];

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, model, REVIEWER_PREAMBLE,
                "Review and correct the Gherkin Feature above. Output only the corrected Gherkin:",
                history, "Reviewer", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(120),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.ollama_reviewer_client(), model, REVIEWER_PREAMBLE,
                "Review and correct the Gherkin Feature above. Output only the corrected Gherkin:",
                history, "Reviewer", file_name, &[],
                status_tx,
                std::time::Duration::from_secs(120),
                cancel_token,
            ).await
        }
    }

    /// Enrich document text with AI-generated descriptions of embedded images.
    ///
    /// Each image is sent to the vision model; the resulting descriptions are
    /// appended to the raw text so the generator LLM has full context.
    #[tracing::instrument(
        name = "llm.enrich_text_with_images",
        skip(self, raw_text, images, status_tx, cancel_token),
        fields(file_name, image_count = images.len())
    )]
    async fn enrich_text_with_images(
        &self,
        raw_text: &str,
        images: &[crate::parser::ExtractedImage],
        file_name: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> String {
        let _ = status_tx.send(format!(
            "👁 [Vision] {}: describing {} image(s) in parallel…",
            file_name,
            images.len(),
        ));

        // Describe all images concurrently
        let futures: Vec<_> = images
            .iter()
            .enumerate()
            .map(|(i, image)| {
                let label = image.label.clone();
                async move {
                    match self.describe_image(image, cancel_token, status_tx).await {
                        Ok(desc) => format!(
                            "[Image {}: {}]\n{}",
                            i + 1,
                            label,
                            desc.trim()
                        ),
                        Err(e) => {
                            warn!(
                                "Failed to describe image {}: {} — using filename as fallback",
                                label, e
                            );
                            format!(
                                "[Image {}: {}]\n(Could not describe image: {})",
                                i + 1,
                                label,
                                e
                            )
                        }
                    }
                }
            })
            .collect();

        let descriptions: Vec<String> = futures::future::join_all(futures).await;

        let _ = status_tx.send(format!(
            "👁 [Vision] {}: all {} image(s) described.",
            file_name,
            images.len(),
        ));

        if descriptions.is_empty() {
            return raw_text.to_string();
        }

        let mut enriched = raw_text.to_string();
        enriched.push_str("\n\n=== Embedded Image Descriptions ===\n\n");
        enriched.push_str(&descriptions.join("\n\n"));
        enriched
    }

    /// Run the pipeline for a group of related files, producing a single merged Gherkin output.
    /// When RAG dynamic_context indexes are configured, cross-file context is
    /// injected automatically by rig-core; otherwise the local ProjectContext
    /// excerpt is used.
    #[tracing::instrument(
        name = "llm.process_group",
        skip(self, members, context, status_tx, cancel_token),
        fields(group_name, member_count = members.len())
    )]
    pub async fn process_group(
        &self,
        group_name: &str,
        members: &[(String, String, String, Vec<crate::parser::ExtractedImage>)],
        context: &ProjectContext,
        status_tx: &std::sync::mpsc::Sender<String>,
        force_regenerate: bool,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Build cache key from all member content + models + mode
        let group_cache_key = {
            let mut parts: Vec<Vec<u8>> = Vec::new();
            parts.push(group_name.as_bytes().to_vec());
            for (name, ftype, text, images) in members {
                parts.push(name.as_bytes().to_vec());
                parts.push(ftype.as_bytes().to_vec());
                parts.push(text.as_bytes().to_vec());
                for img in images {
                    parts.push(img.data.clone());
                }
            }
            parts.push(format!("{:?}", self.mode).into_bytes());
            parts.push(self.generator_model.as_bytes().to_vec());
            parts.push(self.extractor_model.as_bytes().to_vec());
            parts.push(self.reviewer_model.as_bytes().to_vec());
            let refs: Vec<&[u8]> = parts.iter().map(|v| v.as_slice()).collect();
            crate::cache::composite_key(&refs)
        };

        if !force_regenerate {
            if let Some(cached) = self.cache.get_text(crate::cache::NS_LLM, &group_cache_key) {
                let _ = status_tx.send(format!(
                    "📦 [Cache] group {} — loaded from cache", group_name
                ));
                return Ok(cached);
            }
        }

        // ── Step 0: Build merged text and images from all members ──
        let budget = input_budget_for_model(&self.generator_model);
        let mut merged_text = String::new();
        let mut all_images: Vec<&crate::parser::ExtractedImage> = Vec::new();
        let chars_per_member = budget / members.len().max(1);

        for (i, (file_name, file_type, raw_text, images)) in members.iter().enumerate() {
            merged_text.push_str(&format!(
                "=== Document {}: {} ({}) ===\n",
                i + 1,
                file_name,
                file_type
            ));
            let excerpt: String = raw_text.chars().take(chars_per_member).collect();
            merged_text.push_str(&excerpt);
            if raw_text.len() > chars_per_member {
                merged_text.push_str("\n[… content truncated …]\n");
            }
            merged_text.push_str("\n\n");
            all_images.extend(images.iter());
        }

        // ── Step 0b: Describe images with vision model ──
        if !all_images.is_empty() {
            let _ = status_tx.send(format!(
                "👁 [Vision] Describing {} image(s) from group {}…",
                all_images.len(),
                group_name
            ));
            let owned_images: Vec<crate::parser::ExtractedImage> =
                all_images.iter().map(|img| (*img).clone()).collect();
            merged_text =
                self.enrich_text_with_images(&merged_text, &owned_images, group_name, status_tx, cancel_token)
                    .await;
        }

        // ── Pre-compute cross-file context overhead for budget-aware chunking ──
        // Exclude group members from cross-file context
        let member_names: std::collections::HashSet<&str> =
            members.iter().map(|(name, _, _, _)| name.as_str()).collect();
        let exclude: std::collections::HashSet<String> = context
            .file_contents
            .keys()
            .filter(|path| {
                let fname = std::path::Path::new(path.as_str())
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                member_names.contains(fname.as_str())
            })
            .cloned()
            .collect();

        let context_summary = if self.rag_indexes.is_empty() {
            context.build_summary_excluding(&exclude)
        } else {
            String::new()
        };
        let glossary = context.build_glossary();
        let context_overhead = context_summary.len() + glossary.len();

        // ── Chunk-and-merge path for oversized merged groups ──
        let budget_model = if self.mode == PipelineMode::Full {
            if self.extractor_client.is_some() || self.openai_client.is_some() { &self.extractor_model } else { &self.generator_model }
        } else {
            &self.generator_model
        };
        if needs_chunking(&merged_text, budget_model, context_overhead) {
            let result = self.process_file_chunked(
                group_name, "Multi-document group", &merged_text, context, context_overhead, status_tx, cancel_token,
            ).await?;
            self.cache.put_text(crate::cache::NS_LLM, &group_cache_key, &result);
            return Ok(result);
        }

        // ── Step 1: Prepare input for the generator ──
        let summary = if self.mode == PipelineMode::Full {
            let _ = status_tx.send(format!(
                "🔍 [Extractor] Analysing group {}…",
                group_name
            ));
            self.extract_group(group_name, &merged_text, status_tx, cancel_token)
                .await
                .unwrap_or_else(|e| {
                    warn!(
                        "Group extraction failed for {}: {} — falling back to preprocessor",
                        group_name, e
                    );
                    preprocess_text(&merged_text, group_name, "Multi-document group", budget)
                })
        } else {
            let _ = status_tx.send(format!(
                "⚡ [Preprocess] Structuring group {}…",
                group_name
            ));
            preprocess_text(&merged_text, group_name, "Multi-document group", budget)
        };

        // ── Step 2: Generate Gherkin ──
        let _ = status_tx.send(format!(
            "⚙ [Generator] Creating Gherkin for group {}…",
            group_name
        ));

        let gherkin = self
            .generate_group(group_name, &summary, &context_summary, &glossary, status_tx, cancel_token)
            .await?;

        // ── Step 3: Review / refine ──
        let do_review = self.mode != PipelineMode::Fast
            && (self.reviewer_client.is_some() || self.openai_client.is_some());
        let result = if do_review {
            let _ = status_tx.send(format!(
                "✅ [Reviewer] Validating Gherkin for group {}…",
                group_name
            ));
            match self.review(group_name, &gherkin, status_tx, cancel_token).await {
                Ok(refined) => refined,
                Err(e) => {
                    warn!(
                        "Review failed for group {}: {} — using unreviewed output",
                        group_name, e
                    );
                    gherkin
                }
            }
        } else {
            gherkin
        };

        // Store in LLM cache
        self.cache.put_text(crate::cache::NS_LLM, &group_cache_key, &result);

        Ok(result)
    }

    #[tracing::instrument(
        name = "llm.extract_group",
        skip(self, merged_text, status_tx, cancel_token),
        fields(group_name)
    )]
    async fn extract_group(
        &self,
        group_name: &str,
        merged_text: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        let model = self.effective_extractor_model();
        let history = vec![
            Message::user(format!(
                "Group: {group_name}"
            )),
            Message::user(format!(
                "=== Merged Document Content ===\n{merged_text}"
            )),
        ];

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, model, GROUP_EXTRACTOR_PREAMBLE,
                "Produce a single unified structured summary for this document group.",
                history, "Extractor", group_name, &[],
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.ollama_extractor_client(), model, GROUP_EXTRACTOR_PREAMBLE,
                "Produce a single unified structured summary for this document group.",
                history, "Extractor", group_name, &[],
                status_tx,
                std::time::Duration::from_secs(180),
                cancel_token,
            ).await
        }
    }

    #[tracing::instrument(
        name = "llm.generate_group",
        skip(self, summary, context_summary, glossary, status_tx, cancel_token),
        fields(group_name, context_len = context_summary.len())
    )]
    async fn generate_group(
        &self,
        group_name: &str,
        summary: &str,
        context_summary: &str,
        glossary: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        cancel_token: &CancellationToken,
    ) -> Result<String> {
        // Try prefix-cached path first (Ollama only).
        // Skip when RAG dynamic_context is active — same reasoning as generate().
        if self.rag_indexes.is_empty() {
            if let Some(ref cache_mutex) = self.generator_prefix_cache {
                let num_ctx = context_window_for_model(&self.generator_model);
                let cache = cache_mutex.lock().await;
                // Groups use GROUP_GENERATOR_PREAMBLE, not the standard one.
                // The prefix cache is primed with GENERATOR_PREAMBLE. If the
                // glossary matches, the cached prefix still saves glossary
                // recomputation even though the system prompt differs slightly.
                // For simplicity we only use the cache when primed with the
                // matching preamble — which means groups fall through to the
                // rig-core path.  A future optimisation could prime a separate
                // cache for the group preamble.
                if cache.is_primed_for(GENERATOR_PREAMBLE, glossary) {
                    // Build suffix with group-specific framing
                    let mut suffix = String::new();
                    if !context_summary.contains("No prior files") && !context_summary.is_empty() {
                        suffix.push_str(context_summary);
                        suffix.push('\n');
                    }
                    suffix.push_str(&format!(
                        "=== Unified Structured Summary ===\n{summary}\n\n\
                         Generate a single cohesive Gherkin Feature for document group: {group_name}"
                    ));

                    return cache.stream_generate(
                        &suffix,
                        num_ctx,
                        "Generator",
                        group_name,
                        status_tx,
                        std::time::Duration::from_secs(240),
                    ).await;
                }
            }
        }

        // Fallback: multi-turn chat via appropriate backend
        let mut history: Vec<Message> = Vec::new();

        if !glossary.is_empty() {
            history.push(Message::user(glossary.to_owned()));
        }

        if !context_summary.contains("No prior files") && !context_summary.is_empty() {
            history.push(Message::user(context_summary.to_owned()));
        }

        // Fold summary into prompt when RAG active (same pattern as generate()).
        let prompt = if !self.rag_indexes.is_empty() {
            format!(
                "=== Unified Structured Summary ===\n{summary}\n\n\
                 Generate a single cohesive Gherkin Feature for document group: {group_name}"
            )
        } else {
            history.push(Message::user(format!(
                "=== Unified Structured Summary ===\n{summary}"
            )));
            format!("Generate a single cohesive Gherkin Feature for document group: {group_name}")
        };

        if let Some(openai) = &self.openai_client {
            Self::run_openai_chat(
                openai, &self.generator_model, GROUP_GENERATOR_PREAMBLE,
                &prompt, history, "Generator", group_name, &self.rag_indexes,
                status_tx,
                std::time::Duration::from_secs(240),
                cancel_token,
            ).await
        } else {
            Self::run_ollama_chat(
                self.generator_client.as_ref().expect("Ollama generator required"),
                &self.generator_model, GROUP_GENERATOR_PREAMBLE,
                &prompt, history, "Generator", group_name, &self.rag_indexes,
                status_tx,
                std::time::Duration::from_secs(240),
                cancel_token,
            ).await
        }
    }

    /// Describe a single image using the vision model.
    /// Routes to cloud (OpenAI-compatible) or local (Ollama) based on backend.
    /// Results are cached by image content hash + model name.
    #[tracing::instrument(
        name = "llm.describe_image",
        skip(self, image, cancel_token, status_tx),
        fields(image_label = %image.label, image_size_bytes = image.data.len())
    )]
    async fn describe_image(
        &self,
        image: &crate::parser::ExtractedImage,
        cancel_token: &CancellationToken,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        // Check vision cache
        let cache_key = crate::cache::composite_key(&[
            &image.data,
            self.vision_model.as_bytes(),
        ]);
        if let Some(cached) = self.cache.get_text(crate::cache::NS_VISION, &cache_key) {
            return Ok(cached);
        }

        let description = if let (Some(base_url), Some(api_key)) =
            (&self.cloud_vision_base_url, &self.cloud_vision_api_key)
        {
            self.describe_image_cloud(image, base_url, api_key, cancel_token, status_tx).await?
        } else {
            self.describe_image_ollama(image, cancel_token, status_tx).await?
        };

        // Store in cache
        self.cache.put_text(crate::cache::NS_VISION, &cache_key, &description);

        Ok(description)
    }

    /// Describe an image via an OpenAI-compatible chat completions endpoint (streaming).
    /// Sends the image as a base64 data URI in a multimodal user message.
    async fn describe_image_cloud(
        &self,
        image: &crate::parser::ExtractedImage,
        base_url: &str,
        api_key: &str,
        cancel_token: &CancellationToken,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let b64 = BASE64_STANDARD.encode(&image.data);
        let content_type = if image.content_type.is_empty() {
            "image/png"
        } else {
            &image.content_type
        };
        let data_uri = format!("data:{};base64,{}", content_type, b64);
        let label = &image.label;

        let client = reqwest::Client::new();
        let request_fut = client
            .post(format!("{}/chat/completions", base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": self.vision_model,
                "stream": true,
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": VISION_DESCRIBE_PROMPT },
                        { "type": "image_url", "image_url": { "url": data_uri } }
                    ]
                }],
                "max_tokens": 4096
            }))
            .timeout(std::time::Duration::from_secs(120))
            .send();

        let resp = tokio::select! {
            result = request_fut => result
                .with_context(|| format!("Cloud vision API request failed for {}", label))?,
            _ = cancel_token.cancelled() => {
                anyhow::bail!("Vision cancelled for {}", label);
            }
        };

        let mut stream = resp.bytes_stream();
        let mut accumulated = String::new();
        let mut token_count: usize = 0;
        let mut buf = Vec::new();
        let chunk_timeout = std::time::Duration::from_secs(60);

        loop {
            tokio::select! {
                chunk = tokio::time::timeout(chunk_timeout, stream.next()) => {
                    match chunk {
                        Ok(Some(Ok(bytes))) => {
                            buf.extend_from_slice(&bytes);
                            // SSE: lines prefixed with "data: "
                            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line_bytes);
                                let trimmed = line.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }
                                if let Some(data) = trimmed.strip_prefix("data: ") {
                                    if data == "[DONE]" {
                                        return if accumulated.is_empty() {
                                            anyhow::bail!("Cloud vision returned empty response for {}", label)
                                        } else {
                                            Ok(accumulated)
                                        };
                                    }
                                    if let Ok(chunk) = serde_json::from_str::<OpenAIStreamChunk>(data) {
                                        for choice in &chunk.choices {
                                            if let Some(ref text) = choice.delta.content {
                                                accumulated.push_str(text);
                                                token_count += 1;
                                                if token_count % 20 == 0 {
                                                    let _ = status_tx.send(format!(
                                                        "\u{1f441} [Vision] {}: {} tokens\u{2026}", label, token_count
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Ok(Some(Err(e))) => {
                            anyhow::bail!("Cloud vision stream error for {}: {}", label, e);
                        }
                        Ok(None) => {
                            // Stream ended without [DONE]
                            break;
                        }
                        Err(_) => {
                            anyhow::bail!(
                                "Cloud vision stream stalled for {} (no data for {}s after {} tokens)",
                                label, chunk_timeout.as_secs(), token_count
                            );
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    anyhow::bail!("Vision cancelled for {} after {} tokens", label, token_count);
                }
            }
        }

        if accumulated.is_empty() {
            anyhow::bail!("Cloud vision returned empty response for {}", label);
        }
        Ok(accumulated)
    }

    /// Describe an image via the local Ollama `/api/generate` endpoint (streaming).
    async fn describe_image_ollama(
        &self,
        image: &crate::parser::ExtractedImage,
        cancel_token: &CancellationToken,
        status_tx: &std::sync::mpsc::Sender<String>,
    ) -> Result<String> {
        let endpoint_url = &self.vision_endpoint_url;
        let b64 = BASE64_STANDARD.encode(&image.data);
        let label = &image.label;

        let client = reqwest::Client::new();
        let request_fut = client
            .post(format!("{}/api/generate", endpoint_url))
            .json(&serde_json::json!({
                "model": self.vision_model,
                "prompt": VISION_DESCRIBE_PROMPT,
                "images": [b64],
                "stream": true
            }))
            .timeout(std::time::Duration::from_secs(120))
            .send();

        let resp = tokio::select! {
            result = request_fut => result
                .with_context(|| format!("Vision API request failed for {}", label))?,
            _ = cancel_token.cancelled() => {
                anyhow::bail!("Vision cancelled for {}", label);
            }
        };

        let mut stream = resp.bytes_stream();
        let mut accumulated = String::new();
        let mut token_count: usize = 0;
        let mut buf = Vec::new();
        let chunk_timeout = std::time::Duration::from_secs(60);

        loop {
            tokio::select! {
                chunk = tokio::time::timeout(chunk_timeout, stream.next()) => {
                    match chunk {
                        Ok(Some(Ok(bytes))) => {
                            buf.extend_from_slice(&bytes);
                            // Ollama streams newline-delimited JSON
                            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line_bytes);
                                let trimmed = line.trim();
                                if trimmed.is_empty() {
                                    continue;
                                }
                                if let Ok(chunk) = serde_json::from_str::<OllamaStreamGenerateChunk>(trimmed) {
                                    if !chunk.response.is_empty() {
                                        accumulated.push_str(&chunk.response);
                                        token_count += 1;
                                        if token_count % 20 == 0 {
                                            let _ = status_tx.send(format!(
                                                "\u{1f441} [Vision] {}: {} tokens\u{2026}", label, token_count
                                            ));
                                        }
                                    }
                                    if chunk.done {
                                        // Process any remaining buffer
                                        if !buf.is_empty() {
                                            let tail = String::from_utf8_lossy(&buf);
                                            let tail_trimmed = tail.trim();
                                            if !tail_trimmed.is_empty() {
                                                if let Ok(c) = serde_json::from_str::<OllamaStreamGenerateChunk>(tail_trimmed) {
                                                    accumulated.push_str(&c.response);
                                                }
                                            }
                                        }
                                        return if accumulated.is_empty() {
                                            anyhow::bail!("Vision returned empty response for {}", label)
                                        } else {
                                            Ok(accumulated)
                                        };
                                    }
                                }
                            }
                        }
                        Ok(Some(Err(e))) => {
                            anyhow::bail!("Vision stream error for {}: {}", label, e);
                        }
                        Ok(None) => {
                            // Stream ended
                            break;
                        }
                        Err(_) => {
                            anyhow::bail!(
                                "Vision stream stalled for {} (no data for {}s after {} tokens)",
                                label, chunk_timeout.as_secs(), token_count
                            );
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    anyhow::bail!("Vision cancelled for {} after {} tokens", label, token_count);
                }
            }
        }

        if accumulated.is_empty() {
            anyhow::bail!("Vision returned empty response for {}", label);
        }
        Ok(accumulated)
    }
}

// ─────────────────────────────────────────────
// Fast text preprocessor (no LLM)
// ─────────────────────────────────────────────

/// Instantly structure and truncate raw document text for the generator prompt.
/// Replaces the slow LLM extractor in Fast and Standard modes.
///
/// Uses semantic-aware truncation: lines are scored by relevance and
/// high-value content (headings, tables, requirement keywords) is
/// prioritised when the document exceeds the character budget.
fn preprocess_text(raw_text: &str, file_name: &str, file_type: &str, char_budget: usize) -> String {
    let lines: Vec<&str> = raw_text.lines().collect();
    let total_lines = lines.len();

    // Collect non-empty lines with their original index, trimmed.
    let meaningful: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| (i, l.trim()))
        .filter(|(_, l)| !l.is_empty())
        .collect();

    let header = format!(
        "Document: {file_name}\nType: {file_type}\nTotal lines: {total_lines}\n\n"
    );

    let budget = char_budget.saturating_sub(header.len() + 40); // reserve room for truncation note

    // If it fits without truncation, output everything in order.
    let total_chars: usize = meaningful.iter().map(|(_, l)| l.len() + 1).sum();
    if total_chars <= budget {
        let mut result = header;
        for (_, line) in &meaningful {
            result.push_str(line);
            result.push('\n');
        }
        return result;
    }

    // Score each line for semantic relevance.
    let scored: Vec<(usize, &str, u32)> = meaningful
        .iter()
        .map(|&(idx, line)| (idx, line, score_line(line)))
        .collect();

    // Greedily pick lines by score (descending), then by original position.
    let mut indices_by_score: Vec<usize> = (0..scored.len()).collect();
    indices_by_score.sort_by(|&a, &b| {
        scored[b].2.cmp(&scored[a].2).then(scored[a].0.cmp(&scored[b].0))
    });

    let mut selected = Vec::new();
    let mut chars_used: usize = 0;
    for i in indices_by_score {
        let line_cost = scored[i].1.len() + 1;
        if chars_used + line_cost > budget {
            continue;
        }
        selected.push(i);
        chars_used += line_cost;
    }

    // Restore original document order.
    selected.sort_unstable();

    let mut result = header;
    let mut prev_orig_idx: Option<usize> = None;
    for sel_idx in &selected {
        let (orig_idx, line, _) = scored[*sel_idx];
        // Insert separator when lines were skipped.
        if let Some(prev) = prev_orig_idx {
            if orig_idx > prev + 1 {
                result.push_str("[…]\n");
            }
        }
        result.push_str(line);
        result.push('\n');
        prev_orig_idx = Some(orig_idx);
    }

    if selected.len() < scored.len() {
        result.push_str("\n[… content truncated — high-relevance lines retained …]\n");
    }

    result
}

/// Heuristic relevance score for a single line (higher = more important).
fn score_line(line: &str) -> u32 {
    let lower = line.to_lowercase();
    let mut score: u32 = 1; // base score

    // Headings / section markers
    if line.starts_with('#') || lower.starts_with("section") || lower.starts_with("chapter") {
        score += 10;
    }

    // Numbered section headings: "1.", "1.2", "2.3.4 Something"
    if line.len() > 1 && line.as_bytes()[0].is_ascii_digit() && line.contains('.') {
        score += 6;
    }

    // Requirement keywords
    for kw in &["shall", "must", "require", "mandatory", "precondition", "postcondition"] {
        if lower.contains(kw) {
            score += 8;
            break;
        }
    }

    // Action / behaviour keywords
    for kw in &["when", "then", "given", "if", "validate", "verify", "ensure", "submit", "click", "display"] {
        if lower.contains(kw) {
            score += 4;
            break;
        }
    }

    // Actor keywords
    for kw in &["user", "system", "admin", "actor", "service", "module", "role"] {
        if lower.contains(kw) {
            score += 3;
            break;
        }
    }

    // UI dialog / form section markers — critical for field scoping
    for kw in &["create ", "new ", "dialog", "factbox", "consumer", "fast tab", "fasttab"] {
        if lower.contains(kw) {
            score += 6;
            break;
        }
    }

    // Setup vs Runtime boundary markers
    for kw in &["setup", "configuration", "parameter", "category definition", "code list"] {
        if lower.contains(kw) {
            score += 5;
            break;
        }
    }

    // Lifecycle phase markers — essential for correct phase attribution
    for kw in &["on create", "on insert", "on modify", "on delete", "on validate",
                "status change", "category change", "lifecycle", "phase"] {
        if lower.contains(kw) {
            score += 7;
            break;
        }
    }

    // Table-like or structured data (pipes, tabs, separators)
    if line.contains('|') || line.contains('\t') {
        score += 5;
    }

    // Bullet / list items
    if line.starts_with('-') || line.starts_with('*') || line.starts_with("•") {
        score += 3;
    }

    // Image description markers and content — always retain
    if lower.contains("=== embedded image descriptions ===")
        || lower.starts_with("[image ")
        || lower.contains("<inspection") || lower.contains("</inspection")
        || lower.contains("xml schema") || lower.contains("xml structure")
        || lower.contains("xmlns")
    {
        score += 15;
    }

    // Very short lines are usually noise / blank separators — penalise
    if line.len() < 5 {
        score = score.saturating_sub(2);
    }

    score
}

// ─────────────────────────────────────────────
// Chunk-and-merge for oversized documents
// ─────────────────────────────────────────────

struct LlmChunk {
    index: usize,
    total: usize,
    text: String,
}

/// Returns `true` when the document text exceeds the model's effective
/// character budget after reserving space for cross-file context overhead.
fn needs_chunking(text: &str, model: &str, context_overhead: usize) -> bool {
    let budget = input_budget_for_model(model);
    let effective = budget.saturating_sub(context_overhead);
    // Ensure a minimum workable budget (2 000 chars) even with large context
    text.len() > effective.max(2_000)
}

/// Split text into overlapping windows that fit within the model's input budget
/// minus the `context_overhead` (cross-file context + glossary injected per call).
/// Breaks are snapped to line boundaries to avoid cutting mid-sentence.
fn chunk_for_llm(text: &str, model: &str, context_overhead: usize) -> Vec<LlmChunk> {
    let raw_budget = input_budget_for_model(model);
    // Reserve space for context injected into every chunk, with a floor
    let budget = raw_budget.saturating_sub(context_overhead).max(2_000);
    if text.len() <= budget {
        return vec![LlmChunk { index: 0, total: 1, text: text.to_string() }];
    }

    let overlap = budget / 5; // 20% overlap for continuity
    let step = budget - overlap;
    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();

    let mut chunks = Vec::new();
    let mut offset = 0usize;

    while offset < total_chars {
        let end = (offset + budget).min(total_chars);
        let chunk_text: String = chars[offset..end].iter().collect();

        // Snap to line boundary if we're not at the very end
        let actual = if end < total_chars {
            snap_to_line_boundary_llm(&chunk_text)
        } else {
            chunk_text
        };

        if !actual.trim().is_empty() {
            chunks.push(LlmChunk {
                index: chunks.len(),
                total: 0, // patched below
                text: actual,
            });
        }

        offset += step;
    }

    let total = chunks.len();
    for c in &mut chunks {
        c.total = total;
    }
    chunks
}

/// Snap to the last newline in the final 20% of the text to avoid mid-line splits.
fn snap_to_line_boundary_llm(text: &str) -> String {
    let len = text.len();
    let search_start = len.saturating_sub(len / 5);
    if let Some(pos) = text[search_start..].rfind('\n') {
        text[..search_start + pos + 1].to_string()
    } else {
        text.to_string()
    }
}

// ─────────────────────────────────────────────
// Streaming helper
// ─────────────────────────────────────────────

/// Stream a prompt with structured chat history to an agent, accumulating the
/// full response text and sending periodic progress updates via `status_tx`.
/// The model sees each history message as a distinct turn, giving it clearer
/// separation between glossary / context / document content.
#[tracing::instrument(
    name = "llm.stream_chat",
    skip(agent, prompt, chat_history, status_tx, cancel_token),
    fields(stage_name, file_name)
)]
async fn stream_chat_with_progress<M, P>(
    agent: &rig::agent::Agent<M, P>,
    prompt: &str,
    chat_history: Vec<Message>,
    stage_name: &str,
    file_name: &str,
    status_tx: &std::sync::mpsc::Sender<String>,
    timeout: std::time::Duration,
    cancel_token: &CancellationToken,
) -> Result<String>
where
    M: rig::completion::CompletionModel + 'static,
    M::StreamingResponse: rig::completion::GetTokenUsage,
    P: rig::agent::PromptHook<M> + 'static,
{
    // Overall deadline for the entire request (connection + streaming).
    let deadline = tokio::time::Instant::now() + timeout;

    let mut stream = tokio::select! {
        result = tokio::time::timeout(
            timeout,
            agent.stream_prompt(prompt).with_history(chat_history),
        ) => result.with_context(|| format!("{stage_name} timed out after {}s", timeout.as_secs()))?,
        _ = cancel_token.cancelled() => {
            anyhow::bail!("{stage_name} cancelled for {file_name}");
        }
    };

    let mut accumulated = String::new();
    let mut token_count: usize = 0;

    // Per-chunk timeout: if no data arrives for 60s the stream is considered stalled.
    let chunk_timeout = std::time::Duration::from_secs(60);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!(
                "{stage_name} overall deadline exceeded for {file_name} after {token_count} tokens"
            );
        }
        let wait = chunk_timeout.min(remaining);

        tokio::select! {
            chunk = tokio::time::timeout(wait, stream.next()) => {
                match chunk {
                    Ok(Some(item)) => match item {
                        Ok(MultiTurnStreamItem::StreamAssistantItem(
                            StreamedAssistantContent::Text(Text { text }),
                        )) => {
                            accumulated.push_str(&text);
                            token_count += 1;
                            if token_count % 20 == 0 {
                                let _ = status_tx.send(format!(
                                    "🔄 [{stage_name}] {file_name}: {token_count} tokens…"
                                ));
                            }
                        }
                        Ok(MultiTurnStreamItem::FinalResponse(_)) => {
                            break;
                        }
                        Err(e) => {
                            eprintln!("[{stage_name} STREAM ERROR] {file_name}: {e:?}");
                            anyhow::bail!("{stage_name} stream error for {file_name}: {e}");
                        }
                        _ => {}
                    },
                    Ok(None) => {
                        // Stream ended
                        break;
                    }
                    Err(_) => {
                        anyhow::bail!(
                            "{stage_name} stream stalled for {file_name} (no data for {}s after {token_count} tokens)",
                            wait.as_secs()
                        );
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                anyhow::bail!("{stage_name} cancelled for {file_name} after {token_count} tokens");
            }
        }
    }

    if accumulated.is_empty() {
        anyhow::bail!("{stage_name} returned empty response for {file_name}");
    }

    let _ = status_tx.send(format!(
        "✓ [{stage_name}] {file_name}: done ({token_count} tokens, {} chars)",
        accumulated.len()
    ));

    Ok(accumulated)
}

// ─────────────────────────────────────────────
// Connection checks
// ─────────────────────────────────────────────

async fn check_endpoint(endpoint: &OllamaEndpoint) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;

    let port = endpoint.port;
    tokio::task::spawn_blocking(move || {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port)
                .parse()
                .expect("valid address"),
            Duration::from_secs(2),
        )
        .is_ok()
    })
    .await
    .unwrap_or(false)
}

/// Check whether the primary Ollama server is reachable.
pub async fn check_ollama_connection() -> Result<()> {
    if check_endpoint(&ENDPOINT_GENERATOR).await {
        Ok(())
    } else {
        anyhow::bail!(
            "Cannot reach Ollama at {}. Make sure Ollama is running (see docker-compose.yml).",
            ENDPOINT_GENERATOR.url
        )
    }
}
