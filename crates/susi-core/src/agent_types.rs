use std::collections::HashMap;
// Pure agent data types and the GawdAgent trait - no dependency on any
// concrete agent implementation or on gemi/gmcp/daemon, so gemi and gmcp can
// depend on these (and on the AgentMetaRegistry in registry.rs) without
// depending on all of gawd, which is where the concrete agent
// implementations that call into gemi/gmcp/daemon still live.

use crate::susi_error::EaiResult;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GawdAgentInfo {
    pub name: String,
    pub provider: String,
    pub url: String,
    pub rank: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverableAsset {
    pub tier: String,
    pub name: String,
    pub provider: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub categories: Vec<String>,
    pub semantic_anchors: Vec<String>,
    pub base_rank: f32,
    #[serde(default)]
    pub is_core: bool,
}

/// Capacity-capped concurrent string map (DashMap-backed). Evicts an
/// arbitrary entry when full rather than tracking real LRU order.
#[derive(Debug)]
pub struct HighDensityContextStore {
    inner: RwLock<HashMap<String, String>>,
    capacity_limit: usize,
}

impl Default for HighDensityContextStore {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl HighDensityContextStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            capacity_limit: capacity,
        }
    }

    pub fn insert(&self, key: String, value: String) {
        if self.inner.read().unwrap_or_else(|e| e.into_inner()).len() >= self.capacity_limit
            && !self
                .inner
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&key)
        {
            // Mandate: Strict LRU or oldest key removal
            // For DashMap we just remove a random key if we are over capacity.
            //
            // Self-deadlock hazard: `self.inner.iter()` is an unnamed
            // temporary, and DashMap's `Iter` holds its current shard's read
            // lock for the `Iter`'s own lifetime (not just the yielded
            // `RefMulti`'s). Using it directly as an `if let` scrutinee
            // extends that temporary's lifetime to the end of the block
            // (Rust's standard "if let" temporary-extension rule), so the
            // `remove()` below would try to take a write lock on the same
            // shard whose read lock the still-alive `Iter` temporary is
            // holding. Binding to a `let` first forces the `Iter` (and its
            // lock) to drop at the end of this statement, before `remove()`
            // ever runs.
            let key_to_remove = self
                .inner
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .next()
                .map(|(k, _)| k.clone());
            if let Some(key_to_remove) = key_to_remove {
                self.inner
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&key_to_remove);
            }
        }
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, value);
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Ordered snapshot for glass-box inspectability (mission traces / CLI).
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        for r in self.inner.read().unwrap_or_else(|e| e.into_inner()).iter() {
            map.insert(r.0.clone(), r.1.clone());
        }
        map
    }

    /// Restore blackboard entries from a prior snapshot (transaction abort).
    /// Keys present in the live store but absent from `snap` are removed.
    pub fn restore_snapshot(&self, snap: &std::collections::BTreeMap<String, String>) {
        let live_keys: Vec<String> = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        for k in live_keys {
            if !snap.contains_key(&k) {
                self.inner
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&k);
            }
        }
        for (k, v) in snap {
            self.inner
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(k.clone(), v.clone());
        }
    }

    pub fn iter(&self) -> Vec<(String, String)> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.snapshot()).unwrap_or_else(|_| "{}".into())
    }

    /// Persist the live swarm blackboard under the workspace for glass-box review.
    /// Bodies are secret-redacted before write (Mandate 38).
    pub fn persist_inspectable(&self, workspace: &Path) -> std::path::PathBuf {
        let dir = workspace.join(".susi");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("last_blackboard.json");
        let mut entries = Vec::new();
        for (agent, output) in self.snapshot() {
            // The one credential redactor (env-held keys plus the configured
            // governance patterns); a private four-prefix list here missed
            // AIza/AWS/private-key tokens and every unprefixed held key.
            let redacted = crate::susi_config::redact_credentials(&output);
            entries.push(serde_json::json!({
                "agent": agent,
                "bytes": output.len(),
                "output": redacted,
            }));
        }
        let body = serde_json::json!({
            "kind": "mission_blackboard",
            "entries": entries,
            "agent_count": entries.len(),
        });
        // Replaced whole: readers (`susi blackboard`, the crown's glass-box
        // check) parse this file, and a torn write read as corrupt.
        let _ = crate::susi_config::atomic_write_bytes(
            &path,
            serde_json::to_string_pretty(&body)
                .unwrap_or_else(|_| "{}".into())
                .as_bytes(),
        );
        path
    }
}

/// The last mission blackboard [`HighDensityContextStore::persist_inspectable`]
/// wrote under `workspace` (`.susi/last_blackboard.json`), credential-redacted
/// again on read. `susi blackboard show` prints it and MCP `blackboard_show`
/// returns it; the error names the missing file.
pub fn read_persisted_blackboard(workspace: &Path) -> Result<String, String> {
    let path = workspace.join(".susi").join("last_blackboard.json");
    if !path.is_file() {
        return Err(format!(
            "no mission blackboard at {} — run a swarm mission first",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}

/// Shared state agents write their outputs into during a mission.
pub type SwarmBlackboard = Arc<HighDensityContextStore>;
pub type MissionBlackboard = SwarmBlackboard;

/// Trait every swarm agent implements.
pub trait GawdAgent: Send + Sync {
    fn name(&self) -> String;
    fn rank(&self) -> f32;
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> EaiResult<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_inspectable_writes_blackboard_file() {
        let store = HighDensityContextStore::new(8);
        store.insert("SafetyAgent".into(), "clear".into());
        store.insert(
            "ScoutAgent".into(),
            "keys: AIzaSyDemoKey123456 and ghp_demoToken987".into(),
        );
        let dir = std::env::temp_dir().join(format!(
            "susi_bb_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = store.persist_inspectable(&dir);
        assert!(path.is_file());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("SafetyAgent"));
        assert!(text.contains("mission_blackboard"));
        // Every governance pattern is masked, not only the old four.
        assert!(!text.contains("AIzaSyDemoKey123456"), "{text}");
        assert!(!text.contains("ghp_demoToken987"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn high_density_context_store_enforces_capacity_limit_without_deadlock() {
        let store = HighDensityContextStore::new(2);
        assert!(store.is_empty());
        store.insert("k1".into(), "v1".into());
        store.insert("k2".into(), "v2".into());
        store.insert("k3".into(), "v3".into());
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("k3").as_deref(), Some("v3"));

        store.insert("k3".into(), "v3-updated".into());
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("k3").as_deref(), Some("v3-updated"));
    }

    #[test]
    fn snapshot_restore_snapshot_and_to_json_roundtrip_entries() {
        let store = HighDensityContextStore::new(8);
        store.insert("a".into(), "1".into());
        store.insert("b".into(), "2".into());
        let snap = store.snapshot();

        store.insert("c".into(), "3".into());
        store.insert("a".into(), "9".into());
        assert!(store.contains_key("c"));

        store.restore_snapshot(&snap);
        assert!(!store.contains_key("c"));
        assert_eq!(store.get("a").as_deref(), Some("1"));
        assert!(store.to_json().contains("\"b\":\"2\""));
    }

    #[test]
    fn read_persisted_blackboard_errors_when_missing_and_redacts_on_read() {
        let dir = std::env::temp_dir().join(format!(
            "susi_bb_read_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let err = read_persisted_blackboard(&dir).unwrap_err();
        assert!(err.contains("last_blackboard.json"), "{err}");

        let store = HighDensityContextStore::new(4);
        store.insert("PlannerAgent".into(), "plan-ready".into());
        store.persist_inspectable(&dir);
        let read_back = read_persisted_blackboard(&dir).unwrap();
        assert!(read_back.contains("mission_blackboard"));
        assert!(read_back.contains("PlannerAgent"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_high_density_context_store_supports_insert_and_iteration() {
        let store = HighDensityContextStore::default();
        assert!(store.is_empty());
        store.insert("agent-a".into(), "out-a".into());
        store.insert("agent-b".into(), "out-b".into());
        assert_eq!(store.len(), 2);
        let count = store.iter().len();
        assert_eq!(count, 2);
    }
}
