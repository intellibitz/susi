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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn patch(agent: &str, base_rev: u64, paths: &[&str], body: &str) -> Patch {
        Patch {
            agent: agent.to_string(),
            base_rev,
            paths: paths.iter().map(|s| (*s).to_string()).collect(),
            body: body.to_string(),
        }
    }

    #[test]
    fn apply_increments_rev_and_tracks_applied_count_for_disjoint_patches() {
        let mut txn = WorkspaceTxn::new();
        assert_eq!(txn.rev(), 0);
        assert_eq!(
            txn.apply(patch("a1", 0, &["src/a.rs"], "diff-a"), &[]),
            ApplyOutcome::Applied { new_rev: 1 }
        );
        assert_eq!(
            txn.apply(patch("a2", 1, &["src/b.rs"], "diff-b"), &[]),
            ApplyOutcome::Applied { new_rev: 2 }
        );
        assert_eq!(txn.applied_count(), 2);
    }

    #[test]
    fn apply_rejects_stale_base_revision() {
        let mut txn = WorkspaceTxn::new();
        let _ = txn.apply(patch("a1", 0, &["src/a.rs"], "diff-a"), &[]);
        assert_eq!(
            txn.apply(patch("a2", 0, &["src/c.rs"], "diff-c"), &[]),
            ApplyOutcome::StaleBase { current_rev: 1 }
        );
    }

    #[test]
    fn apply_detects_conflicts_against_staging_and_prior_owners() {
        let mut txn = WorkspaceTxn::new();
        let staged = vec![patch("a2", 0, &["src/shared.rs"], "staged")];
        assert_eq!(
            txn.apply(patch("a1", 0, &["src/shared.rs"], "live"), &staged),
            ApplyOutcome::Conflict {
                overlapping: BTreeSet::from(["src/shared.rs".to_string()])
            }
        );

        let _ = txn.apply(patch("a1", 0, &["src/owned.rs"], "first"), &[]);
        assert_eq!(
            txn.apply(patch("a2", 1, &["src/owned.rs"], "second"), &[]),
            ApplyOutcome::Conflict {
                overlapping: BTreeSet::from(["src/owned.rs".to_string()])
            }
        );
    }

    #[test]
    fn unrelated_preserved_retains_only_disjoint_patch_bodies() {
        let applied = vec![
            patch("a1", 0, &["src/clean.rs"], "clean-body"),
            patch("a2", 1, &["src/conflict.rs"], "conflict-body"),
        ];
        let conflicted = BTreeSet::from(["src/conflict.rs".to_string()]);
        assert_eq!(
            unrelated_preserved(&applied, &conflicted),
            vec!["clean-body".to_string()]
        );
    }

    #[test]
    fn apply_allows_same_agent_to_reedit_owned_path_and_round_trips_serde_json() {
        let mut txn = WorkspaceTxn::new();
        let p1 = patch("a1", 0, &["src/lib.rs"], "v1");
        let p2 = patch("a1", 1, &["src/lib.rs"], "v2");
        assert_eq!(
            txn.apply(p1.clone(), std::slice::from_ref(&p1)),
            ApplyOutcome::Applied { new_rev: 1 }
        );
        assert_eq!(txn.apply(p2, &[]), ApplyOutcome::Applied { new_rev: 2 });

        let encoded = serde_json::to_string(&p1).unwrap();
        let decoded: Patch = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, p1);
    }
}
