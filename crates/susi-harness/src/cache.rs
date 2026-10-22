//! In-memory and persistent SHA-256 request/response cache for the metering gateway.
//!
//! Replaces external Redis / LiteLLM proxy caches with a zero-dependency,
//! embedded Rust solution: exact-match hits cost $0.000000 micro-USD and
//! preserve the `LedgerEntry::as_cached()` contract.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// SHA-256 backed in-memory and on-disk completion cache.
pub struct ResponseCache {
    state_dir: Option<PathBuf>,
    in_memory: Arc<RwLock<HashMap<String, Value>>>,
}

impl ResponseCache {
    /// Create a new cache, optionally backed by `<state_dir>/cache/`.
    #[must_use]
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        if let Some(ref dir) = state_dir {
            let cache_dir = dir.join("cache");
            let _ = std::fs::create_dir_all(&cache_dir);
        }
        Self {
            state_dir,
            in_memory: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Compute the deterministic SHA-256 cache key for an inference request.
    #[must_use]
    pub fn compute_key(body: &Value) -> String {
        use sha2::{Digest, Sha256};
        let normalized = json!({
            "model": body.get("model"),
            "messages": body.get("messages"),
            "temperature": body.get("temperature"),
            "max_tokens": body.get("max_tokens").or_else(|| body.get("max_completion_tokens")),
        });
        let bytes = serde_json::to_vec(&normalized).unwrap_or_default();
        hex::encode(Sha256::digest(&bytes))
    }

    /// Look up a cached response by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Value> {
        {
            let guard = self.in_memory.read().unwrap_or_else(|e| e.into_inner());
            if let Some(hit) = guard.get(key) {
                return Some(hit.clone());
            }
        }
        if let Some(ref dir) = self.state_dir {
            let path = dir.join("cache").join(format!("{key}.json"));
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(val) = serde_json::from_slice::<Value>(&bytes) {
                    let mut guard = self.in_memory.write().unwrap_or_else(|e| e.into_inner());
                    guard.insert(key.to_string(), val.clone());
                    return Some(val);
                }
            }
        }
        None
    }

    /// Store a response for a key in memory and on disk.
    pub fn set(&self, key: &str, response: &Value) {
        {
            let mut guard = self.in_memory.write().unwrap_or_else(|e| e.into_inner());
            guard.insert(key.to_string(), response.clone());
        }
        if let Some(ref dir) = self.state_dir {
            let path = dir.join("cache").join(format!("{key}.json"));
            if let Ok(bytes) = serde_json::to_vec_pretty(response) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }

    /// Total number of entries in the in-memory cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.in_memory
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// True when the in-memory cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_computes_deterministic_keys() {
        let req1 = json!({
            "model": "deepseek-flash",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 100
        });
        let req2 = json!({
            "messages": [{"role": "user", "content": "hello"}],
            "model": "deepseek-flash",
            "max_tokens": 100
        });
        assert_eq!(
            ResponseCache::compute_key(&req1),
            ResponseCache::compute_key(&req2)
        );
    }

    #[test]
    fn cache_stores_and_retrieves_in_memory() {
        let cache = ResponseCache::new(None);
        let resp = json!({"choices": [{"message": {"content": "world"}}]});
        cache.set("k1", &resp);
        assert_eq!(cache.get("k1"), Some(resp));
        assert_eq!(cache.get("k2"), None);
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
    }

    #[test]
    fn cache_persists_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let cache1 = ResponseCache::new(Some(temp.path().to_path_buf()));
        let resp = json!({"choices": [{"message": {"content": "persisted"}}]});
        cache1.set("persisted_key", &resp);

        let cache2 = ResponseCache::new(Some(temp.path().to_path_buf()));
        assert_eq!(cache2.get("persisted_key"), Some(resp));
    }
}
