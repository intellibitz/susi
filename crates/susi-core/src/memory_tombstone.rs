//! Propagate memory deletion into derived stores with durable tombstones (VC-201-083).
//!
//! Enforces that:
//! 1. `insert()` checks existing tombstones: late writes for a deleted id are rejected
//!    across authoritative, index, cache, and replica so derived stores cannot be repopulated.
//! 2. Tombstones are durable: they persist across process restarts (via optional persistence
//!    path or export/import/save/load methods).
//! 3. Retrieval cannot resurrect deleted content.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: String,
    pub deleted_unix: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MemoryStores {
    pub authoritative: BTreeMap<String, String>,
    pub semantic_index: BTreeSet<String>,
    pub cache: BTreeMap<String, String>,
    pub replica: BTreeMap<String, String>,
    pub tombstones: BTreeMap<String, Tombstone>,
    /// Ids whose delete was interrupted mid-propagation.
    pub pending_delete: BTreeSet<String>,
    /// Optional backing file path for durable tombstone persistence.
    #[serde(skip)]
    pub persistence_path: Option<PathBuf>,
}

impl MemoryStores {
    /// Create MemoryStores backed by a persistent directory or file for durable tombstones.
    pub fn new_persistent(path: impl Into<PathBuf>) -> Self {
        let mut stores = Self {
            persistence_path: Some(path.into()),
            ..Default::default()
        };
        stores.load_tombstones();
        stores
    }

    /// Load persisted tombstones from disk if a persistence path is configured.
    pub fn load_tombstones(&mut self) {
        if let Some(path) = &self.persistence_path {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(loaded) = serde_json::from_str::<BTreeMap<String, Tombstone>>(&data) {
                    for (k, v) in loaded {
                        self.tombstones.insert(k, v);
                    }
                }
            }
        }
    }

    /// Persist tombstones to disk if a persistence path is configured.
    pub fn persist_tombstones(&self) {
        if let Some(path) = &self.persistence_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(data) = serde_json::to_string(&self.tombstones) {
                let _ = std::fs::write(path, data);
            }
        }
    }

    /// Insert a record into authoritative and derived stores.
    ///
    /// If the id is tombstoned, the write is refused across authoritative,
    /// semantic index, cache, and replica so a late write cannot repopulate
    /// deleted content into derived stores.
    pub fn insert(&mut self, id: &str, value: &str) -> bool {
        if self.tombstones.contains_key(id) {
            // Refuse late write for tombstoned id and ensure no stale content lands
            self.authoritative.remove(id);
            self.semantic_index.remove(id);
            self.cache.remove(id);
            self.replica.remove(id);
            return false;
        }
        self.authoritative.insert(id.to_string(), value.to_string());
        self.semantic_index.insert(id.to_string());
        self.cache.insert(id.to_string(), value.to_string());
        self.replica.insert(id.to_string(), value.to_string());
        true
    }

    /// Begin deletion: tombstone + remove authoritative; mark pending for derived.
    pub fn delete_start(&mut self, id: &str, now: u64) {
        self.tombstones.insert(
            id.to_string(),
            Tombstone {
                id: id.to_string(),
                deleted_unix: now,
            },
        );
        self.persist_tombstones();
        self.authoritative.remove(id);
        self.pending_delete.insert(id.to_string());
    }

    /// Resume interrupted deletion into indexes/caches/replicas.
    pub fn delete_resume(&mut self) {
        let pending: Vec<_> = self.pending_delete.iter().cloned().collect();
        for id in pending {
            self.semantic_index.remove(&id);
            self.cache.remove(&id);
            self.replica.remove(&id);
            self.pending_delete.remove(&id);
        }
    }

    /// Retrieval cannot resurrect deleted content.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&str> {
        if self.tombstones.contains_key(id) {
            return None;
        }
        self.authoritative
            .get(id)
            .map(String::as_str)
            .or_else(|| self.cache.get(id).map(String::as_str))
            .or_else(|| self.replica.get(id).map(String::as_str))
    }

    /// Snapshot durable state for persistence / migration.
    pub fn snapshot_tombstones(&self) -> BTreeMap<String, Tombstone> {
        self.tombstones.clone()
    }

    /// Restore durable state after restart.
    pub fn restore_tombstones(&mut self, tombstones: BTreeMap<String, Tombstone>) {
        self.tombstones.extend(tombstones);
        self.persist_tombstones();
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn insert_populates_all_stores_and_rejects_late_write_after_tombstone() {
        let mut stores = MemoryStores::default();
        assert!(stores.insert("k1", "val-1"));
        assert_eq!(stores.get("k1"), Some("val-1"));
        assert!(stores.semantic_index.contains("k1"));

        stores.delete_start("k1", 100);
        assert!(!stores.insert("k1", "resurrect"));
        assert_eq!(stores.get("k1"), None);
    }

    #[test]
    fn delete_start_and_resume_purge_derived_stores_and_prevent_resurrection() {
        let mut stores = MemoryStores::default();
        assert!(stores.insert("k2", "val-2"));
        stores.delete_start("k2", 200);
        assert_eq!(stores.get("k2"), None);
        assert!(stores.pending_delete.contains("k2"));

        stores.delete_resume();
        assert!(!stores.pending_delete.contains("k2"));
        assert!(!stores.semantic_index.contains("k2"));
        assert!(!stores.cache.contains_key("k2"));
        assert!(!stores.replica.contains_key("k2"));
    }

    #[test]
    fn snapshot_and_restore_tombstones_preserve_deletion_barriers() {
        let mut first = MemoryStores::default();
        first.insert("k3", "val-3");
        first.delete_start("k3", 300);
        let snap = first.snapshot_tombstones();

        let mut second = MemoryStores::default();
        second.restore_tombstones(snap);
        assert!(!second.insert("k3", "late-val"));
        assert_eq!(second.get("k3"), None);
    }

    #[test]
    fn persistent_memory_stores_save_and_reload_tombstones_from_disk() {
        let dir = std::env::temp_dir().join(format!("susi-tomb-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tombstones.json");

        let mut writer = MemoryStores::new_persistent(&path);
        writer.insert("k4", "val-4");
        writer.delete_start("k4", 400);

        let mut reader = MemoryStores::new_persistent(&path);
        assert!(reader.tombstones.contains_key("k4"));
        assert!(!reader.insert("k4", "re-insert"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
