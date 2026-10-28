//! In-memory and persistent SHA-256 request/response cache for the metering gateway.
//!
//! A zero-dependency, embedded cache: exact-match hits cost $0.000000 and
//! preserve the `LedgerEntry::as_cached()` contract.

use serde_json::Value;
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
    ///
    /// The key covers every field that shapes the answer. Only `stream` and
    /// `stream_options` are left out: they change how an answer is delivered,
    /// not what it is, and a cached answer is served either way. A key of
    /// model, messages, temperature and max_tokens alone handed a turn that
    /// declares tools (or a `reasoning_effort`, a `response_format`, `stop`)
    /// the cached answer of a turn that did not.
    #[must_use]
    pub fn compute_key(body: &Value) -> String {
        use sha2::{Digest, Sha256};
        let mut normalized = canonical(body);
        if let Some(object) = normalized.as_object_mut() {
            object.remove("stream");
            object.remove("stream_options");
        }
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

/// `value` with every object's keys in sorted order, so two clients that
/// send the same request with fields in a different order share one key.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
            scalar.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_field_that_shapes_the_answer_is_in_the_key() {
        let plain = json!({
            "model": "gemini-flash",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 40
        });
        let key = ResponseCache::compute_key(&plain);
        for (field, value) in [
            (
                "tools",
                json!([{"type": "function", "function": {"name": "f"}}]),
            ),
            ("tool_choice", json!("required")),
            ("reasoning_effort", json!("none")),
            ("response_format", json!({"type": "json_object"})),
            ("stop", json!(["\n"])),
            ("top_p", json!(0.5)),
        ] {
            let mut shaped = plain.clone();
            shaped[field] = value;
            assert_ne!(ResponseCache::compute_key(&shaped), key, "{field}");
        }
        // Delivery is not shape: a streamed request is served the same answer.
        let mut streamed = plain.clone();
        streamed["stream"] = json!(true);
        streamed["stream_options"] = json!({"include_usage": true});
        assert_eq!(ResponseCache::compute_key(&streamed), key);
        // Nor is the order of a nested object's fields.
        let reordered = json!({
            "max_tokens": 40,
            "messages": [{"content": "hello", "role": "user"}],
            "model": "gemini-flash"
        });
        assert_eq!(ResponseCache::compute_key(&reordered), key);
    }

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
