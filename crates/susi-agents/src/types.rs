// Pure agent data types and the GawdAgent trait - no dependency on any
// concrete agent implementation or on gemi/gmcp/daemon, so gemi and gmcp can
// depend on these (and on the AgentMetaRegistry in registry.rs) without
// depending on all of gawd, which is where the concrete agent
// implementations that call into gemi/gmcp/daemon still live.

use dashmap::DashMap;
use susi_error::EaiResult;

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

    pub fn iter(&self) -> dashmap::iter::Iter<'_, String, String> {
        self.inner.iter()
    }

    pub fn to_json(&self) -> String {
        let mut map = std::collections::HashMap::new();
        for r in self.inner.iter() {
            map.insert(r.key().clone(), r.value().clone());
        }
        serde_json::to_string(&map).unwrap_or_else(|_| "{}".into())
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
