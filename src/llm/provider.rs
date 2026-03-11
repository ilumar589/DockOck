//! Provider abstraction — Ollama (local) vs OpenAI-compatible (cloud).

use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

// ─────────────────────────────────────────────
// Provider backend enum
// ─────────────────────────────────────────────

/// Which LLM backend to use for text generation roles (generator, extractor, reviewer).
/// Vision always stays local (Ollama).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ProviderBackend {
    /// Local Ollama Docker instances (default).
    Ollama,
    /// OpenAI-compatible cloud API.
    Custom {
        name: String,
        base_url: String,
        api_key: String,
    },
}

impl Default for ProviderBackend {
    fn default() -> Self {
        Self::Ollama
    }
}

impl ProviderBackend {
    /// Whether this is a custom (non-Ollama) provider.
    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom { .. })
    }

    /// Display name for the provider.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Ollama => "Ollama (local)",
            Self::Custom { name, .. } => name,
        }
    }
}

// ─────────────────────────────────────────────
// Model limits from custom_providers.json
// ─────────────────────────────────────────────

/// Token limits for a model.
#[derive(Debug, Clone)]
pub struct ModelLimits {
    /// Maximum context window in tokens.
    pub context_tokens: usize,
    /// Maximum output tokens.
    pub max_output_tokens: usize,
}

/// A parsed custom provider with its available models.
#[derive(Debug, Clone)]
pub struct CustomProviderConfig {
    pub key: String,
    pub name: String,
    pub base_url: String,
    pub models: HashMap<String, ModelInfo>,
}

/// Model info including display name and limits.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub display_name: String,
    pub limits: ModelLimits,
}

// ─────────────────────────────────────────────
// JSON schema matching custom_providers.json
// ─────────────────────────────────────────────

#[derive(Deserialize)]
struct JsonRoot {
    provider: HashMap<String, JsonProvider>,
}

#[derive(Deserialize)]
struct JsonProvider {
    name: String,
    options: JsonOptions,
    models: HashMap<String, JsonModel>,
}

#[derive(Deserialize)]
struct JsonOptions {
    #[serde(rename = "baseURL")]
    base_url: String,
}

#[derive(Deserialize)]
struct JsonModel {
    name: String,
    limit: JsonLimits,
}

#[derive(Deserialize)]
struct JsonLimits {
    context: usize,
    output: usize,
}

// ─────────────────────────────────────────────
// Loading
// ─────────────────────────────────────────────

/// Load all custom providers from `custom_providers.json` in the given directory.
/// Returns an empty vec if the file doesn't exist.
pub fn load_custom_providers(dir: &Path) -> Vec<CustomProviderConfig> {
    let path = dir.join("custom_providers.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let root: JsonRoot = match serde_json::from_str(&data) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to parse custom_providers.json: {}", e);
            return Vec::new();
        }
    };

    root.provider
        .into_iter()
        .map(|(key, prov)| CustomProviderConfig {
            key,
            name: prov.name,
            base_url: prov.options.base_url,
            models: prov
                .models
                .into_iter()
                .map(|(id, m)| {
                    (
                        id,
                        ModelInfo {
                            display_name: m.name,
                            limits: ModelLimits {
                                context_tokens: m.limit.context,
                                max_output_tokens: m.limit.output,
                            },
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

/// Try to build a `ProviderBackend::Custom` from the first custom provider
/// config, reading the API key from the environment.
/// Returns `None` if no providers are configured or no API key is set.
pub fn build_custom_backend(configs: &[CustomProviderConfig]) -> Option<ProviderBackend> {
    let config = configs.first()?;

    // Try environment variable: <UPPERCASE_KEY>_API_KEY, e.g. AIARK_API_KEY
    let env_key = format!(
        "{}_API_KEY",
        config.key.to_uppercase().replace(['-', '.'], "_")
    );
    let api_key = std::env::var("AIARK_API_KEY")
        .or_else(|_| std::env::var(&env_key))
        .ok()?;

    if api_key.is_empty() || api_key == "your-api-key-here" {
        return None;
    }

    Some(ProviderBackend::Custom {
        name: config.name.clone(),
        base_url: config.base_url.clone(),
        api_key,
    })
}

/// Look up model limits from custom provider configs.
/// Returns `None` if the model isn't found in any provider.
pub fn custom_model_limits<'a>(configs: &'a [CustomProviderConfig], model_id: &str) -> Option<&'a ModelLimits> {
    for config in configs {
        if let Some(info) = config.models.get(model_id) {
            return Some(&info.limits);
        }
    }
    None
}

/// Return a sorted list of model IDs for the first custom provider.
pub fn custom_model_ids(configs: &[CustomProviderConfig]) -> Vec<String> {
    let Some(config) = configs.first() else {
        return Vec::new();
    };
    let mut ids: Vec<String> = config.models.keys().cloned().collect();
    ids.sort();
    ids
}
