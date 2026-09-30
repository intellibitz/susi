//! Disk budget + LRU eviction for model stores.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreEntry {
    pub id: String,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct DiskBudget {
    pub max_bytes: u64,
    used: u64,
    lru: VecDeque<StoreEntry>,
}

impl DiskBudget {
    #[must_use]
    pub fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            used: 0,
            lru: VecDeque::new(),
        }
    }

    pub fn insert(&mut self, entry: StoreEntry) -> Vec<String> {
        let mut evicted = Vec::new();
        while self.used + entry.bytes > self.max_bytes {
            let Some(old) = self.lru.pop_front() else {
                break;
            };
            self.used = self.used.saturating_sub(old.bytes);
            evicted.push(old.id);
        }
        if self.used + entry.bytes <= self.max_bytes {
            self.used += entry.bytes;
            self.lru.push_back(entry);
        }
        evicted
    }

    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used
    }
}

#[cfg(test)]
mod disk_budget_tests {
    use super::*;

    #[test]
    fn disk_budget_evicts_lru_when_over_cap() {
        let mut b = DiskBudget::new(100);
        assert!(b
            .insert(StoreEntry {
                id: "a".into(),
                bytes: 60,
            })
            .is_empty());
        let ev = b.insert(StoreEntry {
            id: "b".into(),
            bytes: 50,
        });
        assert_eq!(ev, vec!["a".to_string()]);
        assert_eq!(b.used_bytes(), 50);
    }
}
