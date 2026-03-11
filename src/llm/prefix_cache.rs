//! Ollama KV-cache prefix reuse via the `/api/generate` endpoint.
//!
//! The generator model processes the same system preamble + glossary for every
//! file.  By priming the prefix once and feeding back the opaque `context`
//! token array, Ollama skips recomputing attention over the shared prefix on
//! subsequent calls — typically saving 30-50% of generation time per file.
//!
//! **Important**: This only works with `/api/generate`, not `/api/chat`.

use anyhow::{Context, Result};
use sha2::Digest;

/// Response from Ollama `/api/generate` (non-streaming) including KV-cache.
#[derive(serde::Deserialize)]
struct GenerateResponse {
    response: String,
    /// Opaque KV-cache token state.  Feed back to skip prefix recomputation.
    #[serde(default)]
    context: Option<Vec<i64>>,
}

/// Response chunk from a *streaming* `/api/generate` call.
#[derive(serde::Deserialize)]
struct StreamChunk {
    response: String,
    #[serde(default)]
    done: bool,
    /// Only present on the final chunk (done == true).
    #[serde(default)]
    context: Option<Vec<i64>>,
}

/// Holds a cached Ollama KV-cache prefix for a (endpoint, model, prefix_text) triple.
pub struct PrefixCache {
    endpoint_url: String,
    model: String,
    client: reqwest::Client,
    /// The cached KV-cache token array, if primed.
    cached_context: Option<Vec<i64>>,
    /// SHA-256 of the prefix text used to detect invalidation.
    prefix_hash: Option<String>,
    /// The system preamble stored separately (sent as `system` field).
    system_prompt: Option<String>,
}

impl PrefixCache {
    pub fn new(endpoint_url: &str, model: &str) -> Self {
        Self {
            endpoint_url: endpoint_url.to_string(),
            model: model.to_string(),
            client: reqwest::Client::new(),
            cached_context: None,
            prefix_hash: None,
            system_prompt: None,
        }
    }

    /// Prime the cache by sending the shared prefix to Ollama.
    ///
    /// The model processes `system_prompt` + `prefix_text`, producing a
    /// KV-cache state that can be reused for all subsequent calls that share
    /// the same prefix.
    pub async fn prime(
        &mut self,
        system_prompt: &str,
        prefix_text: &str,
        num_ctx: u64,
    ) -> Result<()> {
        let hash = Self::hash_text(system_prompt, prefix_text);

        // Already primed with the same content
        if self.prefix_hash.as_deref() == Some(&hash) && self.cached_context.is_some() {
            return Ok(());
        }

        let resp = self
            .client
            .post(format!("{}/api/generate", self.endpoint_url))
            .json(&serde_json::json!({
                "model": self.model,
                "system": system_prompt,
                "prompt": prefix_text,
                "stream": false,
                "options": { "num_ctx": num_ctx },
            }))
            .send()
            .await
            .context("Prefix cache prime request failed")?;

        let body: GenerateResponse = resp
            .json()
            .await
            .context("Failed to parse prefix cache prime response")?;

        self.cached_context = body.context;
        self.prefix_hash = Some(hash);
        self.system_prompt = Some(system_prompt.to_string());

        Ok(())
    }

    /// Check whether the cache is primed and valid for the given prefix.
    pub fn is_primed_for(&self, system_prompt: &str, prefix_text: &str) -> bool {
        match &self.prefix_hash {
            Some(h) => *h == Self::hash_text(system_prompt, prefix_text) && self.cached_context.is_some(),
            None => false,
        }
    }

    /// Generate with the cached prefix via streaming `/api/generate`.
    ///
    /// The caller supplies only the per-file suffix; the shared prefix is
    /// already baked into `self.cached_context`.  Progress updates are sent
    /// via `status_tx`.
    pub async fn stream_generate(
        &self,
        suffix: &str,
        num_ctx: u64,
        stage_name: &str,
        file_name: &str,
        status_tx: &std::sync::mpsc::Sender<String>,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let ctx = self
            .cached_context
            .as_ref()
            .context("PrefixCache not primed — call prime() first")?;

        let resp = tokio::time::timeout(timeout, async {
            self.client
                .post(format!("{}/api/generate", self.endpoint_url))
                .json(&serde_json::json!({
                    "model": self.model,
                    "prompt": suffix,
                    "system": self.system_prompt.as_deref().unwrap_or(""),
                    "context": ctx,
                    "stream": true,
                    "options": { "num_ctx": num_ctx },
                }))
                .send()
                .await
        })
        .await
        .map_err(|_| anyhow::anyhow!("{stage_name} timed out after {}s", timeout.as_secs()))?
        .context("Prefix-cached generate request failed")?;

        let mut stream = resp.bytes_stream();
        let mut accumulated = String::new();
        let mut token_count: usize = 0;
        let mut buf = Vec::new();

        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let bytes = chunk_result.context("Stream read error")?;
            buf.extend_from_slice(&bytes);

            // Ollama streams newline-delimited JSON
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if let Ok(chunk) = serde_json::from_str::<StreamChunk>(trimmed) {
                    if !chunk.response.is_empty() {
                        accumulated.push_str(&chunk.response);
                        token_count += 1;
                        if token_count % 20 == 0 {
                            let _ = status_tx.send(format!(
                                "🔄 [{stage_name}] {file_name}: {token_count} tokens… (prefix-cached)"
                            ));
                        }
                    }
                    if chunk.done {
                        break;
                    }
                }
            }
        }

        // Process any remaining bytes in buffer
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf);
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                if let Ok(chunk) = serde_json::from_str::<StreamChunk>(trimmed) {
                    accumulated.push_str(&chunk.response);
                }
            }
        }

        if accumulated.is_empty() {
            anyhow::bail!("{stage_name} (prefix-cached) returned empty response for {file_name}");
        }

        let _ = status_tx.send(format!(
            "✓ [{stage_name}] {file_name}: done ({token_count} tokens, {} chars, prefix-cached)",
            accumulated.len()
        ));

        Ok(accumulated)
    }

    fn hash_text(system: &str, prefix: &str) -> String {
        let mut h = sha2::Sha256::new();
        sha2::Digest::update(&mut h, system.as_bytes());
        sha2::Digest::update(&mut h, prefix.as_bytes());
        format!("{:x}", h.finalize())
    }
}
