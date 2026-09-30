//! Transactional concurrent workspace edits (VC-201-029).
//!
//! Stage agent patches against a known base and detect overlapping changes
//! before applying; conflicts are reviewable while unrelated edits apply.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    pub agent: String,
    pub base_rev: u64,
    /// Paths this patch touches.
    pub paths: BTreeSet<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplyOutcome {
    Applied { new_rev: u64 },
    Conflict { overlapping: BTreeSet<String> },
    StaleBase { current_rev: u64 },
}

#[derive(Debug, Default)]
pub struct WorkspaceTxn {
    rev: u64,
    /// Path → last agent that wrote it at this rev.
    owned: BTreeMap<String, String>,
    /// Applied patch bodies (for preservation of unrelated edits).
    applied: Vec<Patch>,
}

impl WorkspaceTxn {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn rev(&self) -> u64 {
        self.rev
    }

    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    /// Stage then commit: reject stale base; on path overlap with another
    /// agent's uncommitted staging set, return a reviewable conflict.
    pub fn apply(&mut self, patch: Patch, staging: &[Patch]) -> ApplyOutcome {
        if patch.base_rev != self.rev {
            return ApplyOutcome::StaleBase {
                current_rev: self.rev,
            };
        }
        let mut overlapping = BTreeSet::new();
        for other in staging {
            if other.agent == patch.agent {
                continue;
            }
            for p in patch.paths.intersection(&other.paths) {
                overlapping.insert(p.clone());
            }
        }
        // Also conflict if another agent already owns a path at this rev
        // and this patch overlaps — concurrent live writers.
        for p in &patch.paths {
            if let Some(owner) = self.owned.get(p) {
                if owner != &patch.agent {
                    overlapping.insert(p.clone());
                }
            }
        }
        if !overlapping.is_empty() {
            return ApplyOutcome::Conflict { overlapping };
        }
        self.rev = self.rev.saturating_add(1);
        for p in &patch.paths {
            self.owned.insert(p.clone(), patch.agent.clone());
        }
        self.applied.push(patch);
        ApplyOutcome::Applied { new_rev: self.rev }
    }
}

/// Paths touched by `a` that do not intersect `b` survive a conflict on `b`.
#[must_use]
pub fn unrelated_preserved(applied: &[Patch], conflicted_paths: &BTreeSet<String>) -> Vec<String> {
    applied
        .iter()
        .filter(|p| p.paths.is_disjoint(conflicted_paths))
        .map(|p| p.body.clone())
        .collect()
}
