//! Content-addressed disk cache for parsed files, vision descriptions, and LLM responses.
//!
//! All cache entries are keyed by a SHA-256 hash of their input content.
//! The cache lives under `<output_dir>/.dockock_cache/<namespace>/`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tracing::{instrument, warn};

/// Compute a hex-encoded SHA-256 hash of the given data.
pub fn content_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Compute a cache key from multiple pieces of data.
pub fn composite_key(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
        hasher.update(b"|"); // separator to avoid collisions
    }
    format!("{:x}", hasher.finalize())
}

/// A file-system-backed cache organized by namespaces.
#[derive(Debug, Clone)]
pub struct DiskCache {
    base_dir: PathBuf,
    enabled: bool,
}

impl DiskCache {
    /// Create a new cache rooted at `<output_dir>/.dockock_cache/`.
    /// If `output_dir` is `None`, the cache is disabled (all ops are no-ops).
    pub fn new(output_dir: Option<&Path>) -> Self {
        match output_dir {
            Some(dir) => Self {
                base_dir: dir.join(".dockock_cache"),
                enabled: true,
            },
            None => Self {
                base_dir: PathBuf::new(),
                enabled: false,
            },
        }
    }

    /// Whether the cache is active (has a valid base directory).
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Retrieve a cached value by namespace and key hash.
    #[instrument(skip(self, key), fields(ns = namespace))]
    pub fn get<T: for<'de> Deserialize<'de>>(&self, namespace: &str, key: &str) -> Option<T> {
        if !self.enabled {
            return None;
        }
        let path = self.base_dir.join(namespace).join(key);
        let data = std::fs::read(&path).ok()?;
        postcard::from_bytes(&data).ok()
    }

    /// Retrieve a cached value as raw string (for text-based caches like vision).
    pub fn get_text(&self, namespace: &str, key: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let path = self.base_dir.join(namespace).join(key);
        std::fs::read_to_string(&path).ok()
    }

    /// Store a value in the cache under a namespace and key hash.
    #[instrument(skip(self, key, value), fields(ns = namespace))]
    pub fn put<T: Serialize>(&self, namespace: &str, key: &str, value: &T) {
        if !self.enabled {
            return;
        }
        let dir = self.base_dir.join(namespace);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("Cache: failed to create dir {}: {}", dir.display(), e);
            return;
        }
        let path = dir.join(key);
        match postcard::to_allocvec(value) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&path, data) {
                    warn!("Cache: failed to write {}: {}", path.display(), e);
                }
            }
            Err(e) => {
                warn!("Cache: failed to serialize: {}", e);
            }
        }
    }

    /// Async version of `get` — offloads the synchronous file read to
    /// tokio's blocking thread pool so it never stalls the async runtime.
    pub async fn get_async<T: for<'de> Deserialize<'de> + Send + 'static>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Option<T> {
        if !self.enabled {
            return None;
        }
        let path = self.base_dir.join(namespace).join(key);
        tokio::task::spawn_blocking(move || {
            let data = std::fs::read(&path).ok()?;
            postcard::from_bytes(&data).ok()
        })
        .await
        .ok()?
    }

    /// Async version of `put` — serialises on the current thread, then
    /// offloads the directory-create + file-write to a blocking thread.
    pub async fn put_async<T: Serialize>(&self, namespace: &str, key: &str, value: &T) {
        if !self.enabled {
            return;
        }
        let dir = self.base_dir.join(namespace);
        let path = dir.join(key);
        let data = match postcard::to_allocvec(value) {
            Ok(d) => d,
            Err(e) => {
                warn!("Cache: failed to serialize: {}", e);
                return;
            }
        };
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                tracing::warn!("Cache: failed to create dir {}: {}", dir.display(), e);
                return;
            }
            if let Err(e) = std::fs::write(&path, data) {
                tracing::warn!("Cache: failed to write {}: {}", path.display(), e);
            }
        })
        .await;
    }

    /// Store raw text in the cache.
    pub fn put_text(&self, namespace: &str, key: &str, value: &str) {
        if !self.enabled {
            return;
        }
        let dir = self.base_dir.join(namespace);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("Cache: failed to create dir {}: {}", dir.display(), e);
            return;
        }
        let path = dir.join(key);
        if let Err(e) = std::fs::write(&path, value) {
            warn!("Cache: failed to write {}: {}", path.display(), e);
        }
    }

    /// Clear all entries in a namespace.
    pub fn clear_namespace(&self, namespace: &str) {
        if !self.enabled {
            return;
        }
        let dir = self.base_dir.join(namespace);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Clear the entire cache.
    pub fn clear_all(&self) {
        if !self.enabled {
            return;
        }
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}

// Cache namespace constants
pub const NS_PARSED: &str = "parsed";
pub const NS_VISION: &str = "vision";
pub const NS_LLM: &str = "llm";
pub const NS_OPENSPEC: &str = "openspec";
pub const NS_EMBEDDING: &str = "embedding";
pub const NS_DEPGRAPH: &str = "depgraph";
