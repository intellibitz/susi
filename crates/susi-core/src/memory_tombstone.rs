//! Propagate memory deletion into derived stores with tombstones (VC-201-083).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub id: String,
    pub deleted_unix: u64,
}

#[derive(Debug, Default)]
pub struct MemoryStores {
    pub authoritative: BTreeMap<String, String>,
    pub semantic_index: BTreeSet<String>,
    pub cache: BTreeMap<String, String>,
    pub replica: BTreeMap<String, String>,
    pub tombstones: BTreeMap<String, Tombstone>,
    /// Ids whose delete was interrupted mid-propagation.
    pub pending_delete: BTreeSet<String>,
}

impl MemoryStores {
    pub fn insert(&mut self, id: &str, value: &str) {
        self.authoritative.insert(id.to_string(), value.to_string());
        self.semantic_index.insert(id.to_string());
        self.cache.insert(id.to_string(), value.to_string());
        self.replica.insert(id.to_string(), value.to_string());
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
}
