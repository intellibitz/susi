//! Replicate only policy-eligible swarm memory (VC-201-084).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedRecord {
    pub id: String,
    pub body: String,
    pub provenance: String,
    pub local_only: bool,
    pub deleted: bool,
}

#[derive(Debug, Default)]
pub struct MemoryReplica {
    pub records: BTreeMap<String, ScopedRecord>,
    pub tombstones: BTreeSet<String>,
}

impl MemoryReplica {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, rec: ScopedRecord) {
        if rec.deleted || self.tombstones.contains(&rec.id) {
            self.tombstones.insert(rec.id.clone());
            self.records.remove(&rec.id);
            return;
        }
        self.records.insert(rec.id.clone(), rec);
    }

    pub fn delete(&mut self, id: &str) {
        self.tombstones.insert(id.to_string());
        self.records.remove(id);
    }

    /// Export only policy-eligible (non-local-only, non-tombstoned) records.
    #[must_use]
    pub fn export_for_peers(&self) -> Vec<ScopedRecord> {
        self.records
            .values()
            .filter(|r| !r.local_only && !r.deleted && !self.tombstones.contains(&r.id))
            .cloned()
            .collect()
    }

    /// Merge peer export: never revive tombstones; skip local-only from peer.
    pub fn merge_from_peer(&mut self, incoming: &[ScopedRecord]) {
        for rec in incoming {
            if rec.local_only {
                continue;
            }
            if self.tombstones.contains(&rec.id) || rec.deleted {
                self.tombstones.insert(rec.id.clone());
                self.records.remove(&rec.id);
                continue;
            }
            match self.records.get(&rec.id) {
                Some(existing) if existing.provenance > rec.provenance => {}
                _ => {
                    self.records.insert(rec.id.clone(), rec.clone());
                }
            }
        }
    }
}

/// Disconnected replicas that exchange exports converge on shared eligible state.
#[must_use]
pub fn converge(a: &MemoryReplica, b: &MemoryReplica) -> (MemoryReplica, MemoryReplica) {
    let mut a2 = MemoryReplica {
        records: a.records.clone(),
        tombstones: a.tombstones.clone(),
    };
    let mut b2 = MemoryReplica {
        records: b.records.clone(),
        tombstones: b.tombstones.clone(),
    };
    a2.merge_from_peer(&b.export_for_peers());
    b2.merge_from_peer(&a.export_for_peers());
    // Also sync tombstones both ways via deleted markers.
    for id in &a.tombstones {
        b2.delete(id);
    }
    for id in &b.tombstones {
        a2.delete(id);
    }
    a2.merge_from_peer(&b2.export_for_peers());
    b2.merge_from_peer(&a2.export_for_peers());
    (a2, b2)
}
