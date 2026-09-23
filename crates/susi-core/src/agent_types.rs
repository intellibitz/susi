// Pure agent data types and the GawdAgent trait - no dependency on any
// concrete agent implementation or on gemi/gmcp/daemon, so gemi and gmcp can
// depend on these (and on the AgentMetaRegistry in registry.rs) without
// depending on all of gawd, which is where the concrete agent
// implementations that call into gemi/gmcp/daemon still live.

use crate::susi_error::EaiResult;
use dashmap::DashMap;

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
    inner: DashMap<String, String>,
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
            inner: DashMap::new(),
            capacity_limit: capacity,
        }
    }

    pub fn insert(&self, key: String, value: String) {
        if self.inner.len() >= self.capacity_limit && !self.inner.contains_key(&key) {
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
            let key_to_remove = self.inner.iter().next().map(|r| r.key().clone());
            if let Some(key_to_remove) = key_to_remove {
                self.inner.remove(&key_to_remove);
            }
        }
        self.inner.insert(key, value);
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key).map(|r| r.value().clone())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Ordered snapshot for glass-box inspectability (mission traces / CLI).
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        for r in self.inner.iter() {
            map.insert(r.key().clone(), r.value().clone());
        }
        map
    }

    /// Restore blackboard entries from a prior snapshot (transaction abort).
    /// Keys present in the live store but absent from `snap` are removed.
    pub fn restore_snapshot(&self, snap: &std::collections::BTreeMap<String, String>) {
        let live_keys: Vec<String> = self.inner.iter().map(|r| r.key().clone()).collect();
        for k in live_keys {
            if !snap.contains_key(&k) {
                self.inner.remove(&k);
            }
        }
        for (k, v) in snap {
            self.inner.insert(k.clone(), v.clone());
        }
    }

    pub fn iter(&self) -> dashmap::iter::Iter<'_, String, String> {
        self.inner.iter()
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
            let redacted = crate::redact::redact_patterns(
                &[
                    "sk-".into(),
                    "ghp_".into(),
                    "github_pat_".into(),
                    "xoxb-".into(),
                ],
                &output,
            );
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
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
        );
        path
    }
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
        let _ = std::fs::remove_dir_all(dir);
    }
}
