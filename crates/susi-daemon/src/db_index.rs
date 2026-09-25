//! Relational Topology Index (Swarm OS Bullet 71)
//!
//! An embedded lightweight relational index (mocked here via in-memory BTree)
//! that stores the topology and historical capability routing tables.

use std::collections::BTreeMap;
use std::sync::RwLock;

/// Represents a topology entry in the relational index.
#[derive(Debug, Clone)]
pub struct TopologyRecord {
    pub cell_id: String,
    pub host_id: String,
    pub capabilities: Vec<String>,
}

/// The relational index for the swarm.
pub struct TopologyIndex {
    records: RwLock<BTreeMap<String, TopologyRecord>>,
}

impl Default for TopologyIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl TopologyIndex {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, record: TopologyRecord) {
        let mut map = self.records.write().unwrap_or_else(|e| e.into_inner());
        map.insert(record.cell_id.clone(), record);
    }

    pub fn query_by_capability(&self, cap: &str) -> Vec<TopologyRecord> {
        let map = self.records.read().unwrap_or_else(|e| e.into_inner());
        map.values()
            .filter(|r| r.capabilities.contains(&cap.to_string()))
            .cloned()
            .collect()
    }
}
