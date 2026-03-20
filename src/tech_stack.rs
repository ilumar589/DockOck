//! Tech stack configuration — load and inject technology stack presets into LLM prompts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Root of the tech_stacks.json config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStackConfig {
    pub stacks: HashMap<String, TechStack>,
}

/// A named technology stack preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStack {
    pub name: String,
    pub description: String,
    pub layers: TechStackLayers,
}

/// The technology layers that compose the stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStackLayers {
    #[serde(default)]
    pub backend_api: Option<LayerSpec>,
    #[serde(default)]
    pub frontend_spa: Option<LayerSpec>,
    #[serde(default)]
    pub database: Option<LayerSpec>,
    #[serde(default)]
    pub cache: Option<LayerSpec>,
    #[serde(default)]
    pub identity: Option<LayerSpec>,
    #[serde(default)]
    pub containers: Option<LayerSpec>,
}

/// Specification for one technology layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerSpec {
    pub technology: String,
    #[serde(default)]
    pub version: Option<String>,
    /// All other key-value pairs (language, framework, orm, patterns, etc.)
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl TechStackConfig {
    /// Load from a JSON file. Returns empty config if file doesn't exist.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("tech_stacks.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse tech_stacks.json: {e}");
                Self { stacks: HashMap::new() }
            }),
            Err(_) => Self { stacks: HashMap::new() },
        }
    }

    /// List of all stack keys for UI dropdown.
    pub fn stack_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.stacks.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Display name for a stack key.
    pub fn display_name(&self, key: &str) -> String {
        self.stacks
            .get(key)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| key.to_string())
    }
}

impl TechStack {
    /// Render this tech stack as a prompt-injection block that can be prepended
    /// to any LLM system message.
    pub fn to_prompt_block(&self) -> String {
        let mut out = String::new();
        out.push_str("=== TARGET TECHNOLOGY STACK ===\n");
        out.push_str(&format!("Stack: {} — {}\n\n", self.name, self.description));

        if let Some(ref be) = self.layers.backend_api {
            out.push_str(&format!(
                "Backend API: {} {}\n",
                be.technology,
                be.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &be.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref fe) = self.layers.frontend_spa {
            out.push_str(&format!(
                "Frontend SPA: {} {}\n",
                fe.technology,
                fe.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &fe.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref db) = self.layers.database {
            out.push_str(&format!(
                "Database: {} {}\n",
                db.technology,
                db.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &db.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref ca) = self.layers.cache {
            out.push_str(&format!(
                "Cache / Session Store: {} {}\n",
                ca.technology,
                ca.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &ca.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref id) = self.layers.identity {
            out.push_str(&format!(
                "Identity & Auth: {} {}\n",
                id.technology,
                id.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &id.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }
        if let Some(ref ct) = self.layers.containers {
            out.push_str(&format!(
                "Container Runtime: {} {}\n",
                ct.technology,
                ct.version.as_deref().unwrap_or("")
            ));
            for (k, v) in &ct.extra {
                out.push_str(&format!("  {}: {}\n", k, format_value(v)));
            }
        }

        out.push_str("\nIMPORTANT: All generated documentation MUST target this specific stack:\n");
        out.push_str(
            "- Database schemas: use the SQL dialect of the specified database engine\n",
        );
        out.push_str(
            "- Data models: use the type system and conventions of the backend language\n",
        );
        out.push_str("- API contracts: use the patterns of the specified backend framework\n");
        out.push_str(
            "- Frontend components: reference the specified UI framework and component library\n",
        );
        out.push_str(
            "- Architecture diagrams: show the actual technology names, not generic labels\n",
        );
        out.push_str(
            "- Auth flows: use the specified identity provider's protocol and endpoints\n",
        );
        out.push_str("- Deployment: use the specified container runtime and orchestration\n");
        out.push_str("=== END TECHNOLOGY STACK ===\n\n");
        out
    }
}

/// Format a serde_json::Value for prompt display.
fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}
