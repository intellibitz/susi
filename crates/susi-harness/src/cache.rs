//! In-memory and persistent SHA-256 request/response cache for the metering gateway.
//!
//! A zero-dependency, embedded cache: exact-match hits cost $0.000000 and
//! preserve the `LedgerEntry::as_cached()` contract.
//!
//! The cache is bounded ([`CacheBounds`]). A gateway shared by many agents for
//! days serves mostly unique turns (each one's history is longer than the
//! last), and an unbounded map and directory kept every answer it ever served:
//! the process grew until it was killed, and the directory beside the ledger
//! without limit.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How much the cache keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheBounds {
    /// Answers kept in memory; the least recently used goes first.
    pub memory_entries: usize,
    /// Bytes of answers (as JSON) kept in memory.
    pub memory_bytes: usize,
    /// Answer files kept on disk; the least recently written go first.
    pub disk_entries: usize,
    /// Bytes of answer files kept on disk.
    pub disk_bytes: u64,
    /// An answer larger than this is not cached at all.
    pub entry_bytes: usize,
}

impl Default for CacheBounds {
    fn default() -> Self {
        Self {
            memory_entries: 2_048,
            memory_bytes: 64 * 1024 * 1024,
            disk_entries: 20_000,
            disk_bytes: 512 * 1024 * 1024,
            entry_bytes: 2 * 1024 * 1024,
        }
    }
}

/// The disk is trimmed to its bounds on startup and after this many writes.
const PRUNE_EVERY: u64 = 256;

/// SHA-256 backed in-memory and on-disk completion cache.
pub struct ResponseCache {
    state_dir: Option<PathBuf>,
    bounds: CacheBounds,
    in_memory: Mutex<Memory>,
    writes: AtomicU64,
}

/// The in-memory half: answers by key, with their size and last use.
#[derive(Default)]
struct Memory {
    entries: HashMap<String, Slot>,
    bytes: usize,
    clock: u64,
}

struct Slot {
    value: Value,
    bytes: usize,
    used: u64,
}

impl Memory {
    fn get(&mut self, key: &str) -> Option<Value> {
        self.clock += 1;
        let clock = self.clock;
        let slot = self.entries.get_mut(key)?;
        slot.used = clock;
        Some(slot.value.clone())
    }

    /// Keep `value`, evicting the least recently used answers to make room.
    /// An answer larger than the whole budget is not kept in memory.
    fn insert(&mut self, key: &str, value: &Value, bytes: usize, bounds: CacheBounds) {
        if let Some(old) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        if bounds.memory_entries == 0 || bytes > bounds.memory_bytes {
            return;
        }
        while self.entries.len() >= bounds.memory_entries
            || self.bytes.saturating_add(bytes) > bounds.memory_bytes
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, slot)| slot.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
            }
        }
        self.clock += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key.to_string(),
            Slot {
                value: value.clone(),
                bytes,
                used: self.clock,
            },
        );
    }
}

impl ResponseCache {
    /// Create a new cache with the default [`CacheBounds`], optionally backed
    /// by `<state_dir>/cache/`.
    #[must_use]
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        Self::with_bounds(state_dir, CacheBounds::default())
    }

    /// Create a new cache that keeps no more than `bounds`, trimming what an
    /// earlier run left on disk to them.
    #[must_use]
    pub fn with_bounds(state_dir: Option<PathBuf>, bounds: CacheBounds) -> Self {
        if let Some(ref dir) = state_dir {
            let cache_dir = dir.join("cache");
            let _ = std::fs::create_dir_all(&cache_dir);
        }
        let cache = Self {
            state_dir,
            bounds,
            in_memory: Mutex::new(Memory::default()),
            writes: AtomicU64::new(0),
        };
        cache.prune_disk();
        cache
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
        if let Some(hit) = self.memory().get(key) {
            return Some(hit);
        }
        let path = self.path_of(key)?;
        let bytes = std::fs::read(&path).ok()?;
        let value = serde_json::from_slice::<Value>(&bytes).ok()?;
        // A hit is a use: the disk is trimmed oldest-written first, so a file
        // still being served is written forward instead of aging out.
        if let Ok(file) = std::fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        self.memory().insert(key, &value, bytes.len(), self.bounds);
        Some(value)
    }

    /// Store a response for a key in memory and on disk.
    pub fn set(&self, key: &str, response: &Value) {
        let Ok(bytes) = serde_json::to_vec(response) else {
            return;
        };
        if bytes.len() > self.bounds.entry_bytes {
            return;
        }
        self.memory()
            .insert(key, response, bytes.len(), self.bounds);
        if let Some(path) = self.path_of(key) {
            let _ = std::fs::write(path, bytes);
            if self.writes.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == PRUNE_EVERY - 1 {
                self.prune_disk();
            }
        }
    }

    /// Total number of entries in the in-memory cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.memory().entries.len()
    }

    /// True when the in-memory cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn memory(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.in_memory.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path_of(&self, key: &str) -> Option<PathBuf> {
        let dir = self.state_dir.as_ref()?;
        Some(dir.join("cache").join(format!("{key}.json")))
    }

    /// Delete the least recently written answer files until the directory is
    /// within its bounds. Another gateway may share the directory; a file it
    /// removed first, or holds open, is simply skipped.
    fn prune_disk(&self) {
        let Some(dir) = self.state_dir.as_ref().map(|dir| dir.join("cache")) else {
            return;
        };
        let Ok(listing) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = listing
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                meta.is_file().then(|| {
                    (
                        meta.modified().unwrap_or(UNIX_EPOCH),
                        meta.len(),
                        entry.path(),
                    )
                })
            })
            .collect();
        let mut count = files.len();
        let mut bytes: u64 = files.iter().map(|(_, len, _)| *len).sum();
        let within = |count: usize, bytes: u64| {
            count <= self.bounds.disk_entries && bytes <= self.bounds.disk_bytes
        };
        if within(count, bytes) {
            return;
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        for (_, len, path) in files {
            if within(count, bytes) {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                count -= 1;
                bytes = bytes.saturating_sub(len);
            }
        }
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
    use std::time::Duration;

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

    fn answer(text: &str) -> Value {
        json!({"choices": [{"message": {"content": text}}]})
    }

    /// Memory holds the answers in use, not every answer ever served.
    #[test]
    fn memory_keeps_the_most_recently_used_answers_within_its_bounds() {
        let bounds = CacheBounds {
            memory_entries: 2,
            ..CacheBounds::default()
        };
        let cache = ResponseCache::with_bounds(None, bounds);
        cache.set("k1", &answer("one"));
        cache.set("k2", &answer("two"));
        assert!(cache.get("k1").is_some(), "k1 is now the most recent");
        cache.set("k3", &answer("three"));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("k2"), None, "the least recently used went");
        assert_eq!(cache.get("k1"), Some(answer("one")));
        assert_eq!(cache.get("k3"), Some(answer("three")));

        // A byte budget evicts as well, and an answer over it stays out.
        let size = serde_json::to_vec(&answer("one")).unwrap().len();
        let bounds = CacheBounds {
            memory_bytes: size * 2,
            ..CacheBounds::default()
        };
        let cache = ResponseCache::with_bounds(None, bounds);
        cache.set("a", &answer("one"));
        cache.set("b", &answer("two"));
        cache.set("c", &answer("six"));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("a"), None);
        cache.set("big", &answer(&"x".repeat(size * 2)));
        assert_eq!(cache.get("big"), None);
    }

    #[test]
    fn an_answer_over_the_entry_bound_is_not_cached() {
        let temp = tempfile::tempdir().unwrap();
        let bounds = CacheBounds {
            entry_bytes: 64,
            ..CacheBounds::default()
        };
        let cache = ResponseCache::with_bounds(Some(temp.path().to_path_buf()), bounds);
        cache.set("huge", &answer(&"x".repeat(128)));
        assert_eq!(cache.get("huge"), None);
        assert!(!temp.path().join("cache").join("huge.json").exists());
    }

    /// The directory beside the ledger is trimmed oldest-written first when
    /// a gateway starts, so it no longer grows for as long as one runs.
    #[test]
    fn the_disk_drops_its_least_recently_written_answers_past_its_bounds() {
        let temp = tempfile::tempdir().unwrap();
        let unbounded = ResponseCache::new(Some(temp.path().to_path_buf()));
        let now = SystemTime::now();
        for (age, key) in [(5, "k5"), (4, "k4"), (3, "k3"), (2, "k2"), (1, "k1")] {
            unbounded.set(key, &answer(key));
            let path = temp.path().join("cache").join(format!("{key}.json"));
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(now - Duration::from_secs(age * 60))
                .unwrap();
        }

        let bounds = CacheBounds {
            disk_entries: 3,
            ..CacheBounds::default()
        };
        let restarted = ResponseCache::with_bounds(Some(temp.path().to_path_buf()), bounds);

        let mut kept: Vec<String> = std::fs::read_dir(temp.path().join("cache"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(kept, ["k1.json", "k2.json", "k3.json"]);
        assert_eq!(restarted.get("k1"), Some(answer("k1")));
        assert_eq!(restarted.get("k5"), None);
    }
}
