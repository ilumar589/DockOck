//! HTTP client for the containerised OpenSpec service.
//!
//! The service accepts Gherkin text and returns OpenSpec change artifacts
//! (proposal, spec, tasks, design) as a JSON bundle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default endpoint for the OpenSpec service container.
pub const DEFAULT_OPENSPEC_URL: &str = "http://localhost:11438";

/// Request payload sent to `POST /generate`.
#[derive(Serialize)]
struct GenerateRequest<'a> {
    change_name: &'a str,
    gherkin: &'a str,
    generate_proposal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ollama_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ollama_model: Option<&'a str>,
}

/// Response returned by `POST /generate`.
#[derive(Debug, Clone, Deserialize)]
pub struct GenerateResponse {
    #[allow(dead_code)]
    pub success: bool,
    pub change_name: String,
    pub feature_title: String,
    #[allow(dead_code)]
    pub scenario_count: usize,
    /// Relative path → markdown content for each artifact.
    pub artifacts: HashMap<String, String>,
    /// Optional CLI validation output (may be `null` if CLI unavailable).
    #[allow(dead_code)]
    pub validation: Option<serde_json::Value>,
}

/// Result of an OpenSpec export for one Gherkin document.
#[derive(Debug, Clone)]
pub struct OpenSpecExportResult {
    #[allow(dead_code)]
    pub change_name: String,
    #[allow(dead_code)]
    pub feature_title: String,
    pub artifacts: HashMap<String, String>,
    #[allow(dead_code)]
    pub saved_paths: Vec<PathBuf>,
}

/// Check whether the OpenSpec service is reachable.
pub async fn check_service(base_url: &str) -> Result<(), String> {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| format!("OpenSpec service unreachable: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("OpenSpec service returned status {}", resp.status()))
    }
}

/// Post Gherkin text to the OpenSpec service and receive generated artifacts.
pub async fn generate(
    base_url: &str,
    change_name: &str,
    gherkin_text: &str,
    generate_proposal: bool,
) -> Result<GenerateResponse, String> {
    let url = format!("{}/generate", base_url.trim_end_matches('/'));
    let body = GenerateRequest {
        change_name,
        gherkin: gherkin_text,
        generate_proposal,
        ollama_url: None,
        ollama_model: None,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("OpenSpec request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("OpenSpec service error ({}): {}", status, text));
    }

    resp.json::<GenerateResponse>()
        .await
        .map_err(|e| format!("Failed to parse OpenSpec response: {}", e))
}

/// Save artifacts from a `GenerateResponse` into the given output directory.
///
/// Files are written under `<output_dir>/openspec/<change_name>/`.
pub fn save_artifacts(
    output_dir: &Path,
    response: &GenerateResponse,
) -> Result<Vec<PathBuf>, String> {
    let base = output_dir
        .join("openspec")
        .join(&response.change_name);

    std::fs::create_dir_all(&base)
        .map_err(|e| format!("Failed to create directory {}: {}", base.display(), e))?;

    let mut saved = Vec::new();
    for (rel_path, content) in &response.artifacts {
        let full_path = base.join(rel_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&full_path, content)
            .map_err(|e| format!("Failed to write {}: {}", full_path.display(), e))?;
        saved.push(full_path);
    }

    Ok(saved)
}
