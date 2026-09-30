//! Disk budget for downloaded models — tracks what susi fetched, when each
//! model was last used, and evicts least-recently-used *susi-owned* files
//! when a new pull would exceed the configured budget. Models the user
//! placed in the store themselves are never evicted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use susi_error::EaiResult;

/// A tracked model artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// Stable id (HF repo + file, or absolute path).
    pub id: String,
    /// On-disk size in bytes.
    pub bytes: u64,
    /// Unix seconds the model last served inference or was touched.
    pub last_used_unix: i64,
    /// `true` when susi downloaded it; `false` = user-managed, never evict.
    pub owned_by_susi: bool,
}

#[derive(Debug, Default)]
pub struct DiskBudget {
    /// Maximum bytes the managed store may hold.
    pub budget_bytes: u64,
    entries: BTreeMap<String, ModelEntry>,
}

/// Result of planning space for an incoming download.
#[derive(Debug, Clone, PartialEq)]
pub struct Eviction {
    /// Ids to delete, least-recently-used first.
    pub evict: Vec<String>,
    /// Bytes freed by the eviction list.
    pub freed: u64,
    /// Bytes the store holds after eviction + the new download.
    pub after_bytes: u64,
    /// `false` when even evicting every owned file cannot make room — the
    /// download must not proceed.
    pub feasible: bool,
}

impl DiskBudget {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            entries: BTreeMap::new(),
        }
    }

    pub fn track(&mut self, e: ModelEntry) {
        self.entries.insert(e.id.clone(), e);
    }

    /// Record that `id` served a request at `unix` (bump LRU position).
    pub fn touch(&mut self, id: &str, unix: i64) {
        if let Some(e) = self.entries.get_mut(id) {
            e.last_used_unix = unix;
        }
    }

    /// Bytes currently tracked (susi-owned + user files).
    pub fn used_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.bytes).sum()
    }

    /// Decide what to delete so `incoming_bytes` fits the budget. Evicts
    /// susi-owned entries oldest-first; user-managed files are excluded and
    /// their bytes still count against the budget.
    pub fn plan_eviction(&self, incoming_bytes: u64) -> Eviction {
        let mut evict = Vec::new();
        let mut freed = 0u64;
        let mut lru: Vec<&ModelEntry> = self.entries.values().filter(|e| e.owned_by_susi).collect();
        lru.sort_by_key(|e| e.last_used_unix);
        for e in lru {
            if self.used_bytes() - freed + incoming_bytes <= self.budget_bytes {
                break;
            }
            evict.push(e.id.clone());
            freed += e.bytes;
        }
        let after = self.used_bytes().saturating_sub(freed) + incoming_bytes;
        Eviction {
            evict,
            freed,
            after_bytes: after,
            feasible: after <= self.budget_bytes,
        }
    }

    /// Remove ids from tracking (after the caller deleted them).
    pub fn forget(&mut self, ids: &[String]) {
        for id in ids {
            self.entries.remove(id);
        }
    }

    /// Serialise the tracker for persistence (`config/models-budget.json`).
    pub fn to_json(&self) -> EaiResult<String> {
        serde_json::to_string_pretty(&self.entries.values().collect::<Vec<_>>())
            .map_err(|e| susi_error::EaiError::config(e.to_string()))
    }

    pub fn from_json(budget_bytes: u64, json: &str) -> EaiResult<Self> {
        let v: Vec<ModelEntry> = serde_json::from_str(json)
            .map_err(|e| susi_error::EaiError::config(format!("budget store: {e}")))?;
        let mut b = Self::new(budget_bytes);
        for e in v {
            b.track(e);
        }
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn entry(id: &str, gib: u64, last: i64, owned: bool) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            bytes: gib * GIB,
            last_used_unix: last,
            owned_by_susi: owned,
        }
    }

    #[test]
    fn disk_budget_no_eviction_when_it_fits() {
        let mut b = DiskBudget::new(50 * GIB);
        b.track(entry("a", 10, 100, true));
        let ev = b.plan_eviction(5 * GIB);
        assert!(ev.evict.is_empty());
        assert!(ev.feasible);
        assert_eq!(ev.after_bytes, 15 * GIB);
    }

    #[test]
    fn disk_budget_evicts_lru_until_it_fits() {
        let mut b = DiskBudget::new(40 * GIB);
        b.track(entry("old", 10, 100, true));
        b.track(entry("mid", 10, 200, true));
        b.track(entry("new", 10, 300, true));
        // incoming 15 GiB: need to drop ~5 GiB → evict `old`; check `mid` too
        // since 30+15 > 40 until one evict (30-10+15=35 ≤ 40 → just `old`).
        let ev = b.plan_eviction(15 * GIB);
        assert_eq!(ev.evict, ["old"]);
        assert!(ev.feasible);
    }

    #[test]
    fn disk_budget_evicts_multiple_when_needed() {
        let mut b = DiskBudget::new(40 * GIB);
        b.track(entry("a", 15, 100, true));
        b.track(entry("b", 15, 200, true));
        b.track(entry("c", 5, 300, true));
        // incoming 20: evict a (35-15+20=40 ≤ 40) — only `a` needed.
        let ev = b.plan_eviction(20 * GIB);
        assert_eq!(ev.evict, ["a"]);
        assert!(ev.feasible);
        // incoming 25: evict a then b (35-30+25=30 ≤ 40).
        let ev = b.plan_eviction(25 * GIB);
        assert_eq!(ev.evict, ["a", "b"]);
        assert!(ev.feasible);
    }

    #[test]
    fn disk_budget_never_evicts_user_files() {
        let mut b = DiskBudget::new(20 * GIB);
        b.track(entry("mine", 15, 1, false));
        b.track(entry("susi-a", 3, 10, true));
        // incoming 4 GiB: 18+4=22 > 20 → evict susi-a only (user file stays).
        let ev = b.plan_eviction(4 * GIB);
        assert_eq!(ev.evict, ["susi-a"]);
        assert!(ev.feasible);
        assert_eq!(ev.after_bytes, 19 * GIB);
    }

    #[test]
    fn disk_budget_infeasible_when_user_files_fill_budget() {
        let mut b = DiskBudget::new(20 * GIB);
        b.track(entry("mine", 18, 1, false));
        b.track(entry("susi-a", 1, 10, true));
        // incoming 5: even evicting everything susi-owned leaves 18+5=23 > 20.
        let ev = b.plan_eviction(5 * GIB);
        assert!(!ev.feasible);
        assert_eq!(ev.evict, ["susi-a"]); // still reports what could be freed
    }

    #[test]
    fn disk_budget_touch_reorders_lru() {
        let mut b = DiskBudget::new(40 * GIB);
        b.track(entry("a", 10, 100, true));
        b.track(entry("b", 10, 200, true));
        b.track(entry("c", 10, 300, true));
        b.touch("a", 400); // `a` is now freshest
        let ev = b.plan_eviction(15 * GIB);
        assert_eq!(ev.evict, ["b"]);
    }

    #[test]
    fn disk_budget_json_round_trip() {
        let mut b = DiskBudget::new(40 * GIB);
        b.track(entry("a", 10, 100, true));
        b.track(entry("u", 5, 50, false));
        let s = b.to_json().unwrap();
        let b2 = DiskBudget::from_json(40 * GIB, &s).unwrap();
        assert_eq!(b2.used_bytes(), 15 * GIB);
    }
}
