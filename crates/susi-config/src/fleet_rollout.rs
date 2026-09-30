//! Staged fleet configuration rollout (VC-201-068).
//!
//! A pinned revision rolls out to a canary wave first, then bounded waves.
//! A wave only opens once every already-started node is healthy on the
//! pinned revision; an unhealthy node pauses the rollout, keeps its
//! previous revision as rollback state, and blocks later waves entirely.

use crate::susi_error::{eai_bail as bail, EaiResult};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutStatus {
    /// Canary or a middle wave is in flight.
    Running,
    /// A node reported unhealthy; rollout halts until repaired/rolled back.
    Paused,
    /// Every node runs the pinned revision healthily.
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeState {
    /// Revision currently running on the node (None = never configured).
    pub current: Option<String>,
    /// The revision this rollout pins it to.
    pub target: String,
    /// Revision to restore on rollback — what ran before this rollout.
    pub rollback_to: Option<String>,
    pub healthy: bool,
    /// Wave index this node belongs to (0 = canary).
    pub wave: usize,
}

/// `nodes` are `(id, current_revision)`; `canary` picks wave-0 members.
/// Remaining nodes fill waves of at most `wave_size`.
pub struct Rollout {
    pub revision: String,
    pub nodes: BTreeMap<String, NodeState>,
    pub wave_count: usize,
    pub open_wave: usize,
    pub status: RolloutStatus,
}

impl Rollout {
    pub fn new(
        revision: &str,
        nodes: &[(String, Option<String>)],
        canary: &[String],
        wave_size: usize,
    ) -> EaiResult<Self> {
        if revision.is_empty() {
            bail!("rollout needs a pinned revision");
        }
        if wave_size == 0 {
            bail!("wave_size must be at least 1");
        }
        let canary_set: std::collections::BTreeSet<&String> = canary.iter().collect();
        if canary_set
            .iter()
            .any(|c| !nodes.iter().any(|(id, _)| id == *c))
        {
            bail!("canary names a node not in the fleet");
        }
        let mut map = BTreeMap::new();
        let mut wave = 0usize;
        let mut in_wave = 0usize;
        for (id, current) in nodes {
            let w = if canary_set.contains(id) {
                0
            } else {
                if in_wave == 0 {
                    wave = 1;
                }
                if in_wave >= wave_size {
                    wave += 1;
                    in_wave = 0;
                }
                in_wave += 1;
                wave
            };
            map.insert(
                id.clone(),
                NodeState {
                    current: current.clone(),
                    target: revision.to_string(),
                    rollback_to: current.clone(),
                    healthy: true,
                    wave: w,
                },
            );
        }
        Ok(Self {
            revision: revision.to_string(),
            nodes: map,
            wave_count: if wave == 0 { 1 } else { wave + 1 },
            open_wave: 0,
            status: RolloutStatus::Running,
        })
    }

    /// Nodes allowed to receive the pinned revision right now — the open
    /// wave, nothing beyond it. Empty while paused: a failing wave stops
    /// further application until repaired or rolled back.
    #[must_use]
    pub fn pending(&self) -> Vec<String> {
        if self.status == RolloutStatus::Paused {
            return Vec::new();
        }
        self.nodes
            .iter()
            .filter(|(_, n)| {
                n.wave <= self.open_wave && n.current.as_deref() != Some(&self.revision)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Report a node as having applied `revision` and its health. Any
    /// unhealthy report pauses the rollout; a healthy report on the pinned
    /// revision may complete the open wave and open the next.
    pub fn report(&mut self, id: &str, healthy: bool) -> EaiResult<()> {
        let Some(node) = self.nodes.get_mut(id) else {
            bail!("unknown node {id}");
        };
        node.healthy = healthy;
        if !healthy {
            self.status = RolloutStatus::Paused;
            return Ok(());
        }
        if self.open_wave_done() && self.open_wave + 1 < self.wave_count {
            self.open_wave += 1;
        }
        if self.all_pinned_healthy() {
            self.status = RolloutStatus::Done;
        }
        Ok(())
    }

    /// Mark a node's applied revision (the operator's apply step).
    pub fn mark_applied(&mut self, id: &str, revision: &str) -> EaiResult<()> {
        if revision != self.revision {
            bail!(
                "node {id} applied {revision}, expected pinned {}",
                self.revision
            );
        }
        let Some(node) = self.nodes.get_mut(id) else {
            bail!("unknown node {id}");
        };
        node.current = Some(revision.to_string());
        Ok(())
    }

    /// Restore a node to its pre-rollout revision and report the state.
    /// The rollout stays paused; an operator may resume with `resume`.
    pub fn rollback(&mut self, id: &str) -> EaiResult<Option<String>> {
        let Some(node) = self.nodes.get_mut(id) else {
            bail!("unknown node {id}");
        };
        node.current = node.rollback_to.clone();
        node.healthy = true;
        Ok(node.rollback_to.clone())
    }

    /// Resume after repairing or rolling back unhealthy nodes. Refuses to
    /// resume while any already-applied node is still unhealthy.
    pub fn resume(&mut self) -> EaiResult<()> {
        if self.nodes.values().any(|n| !n.healthy) {
            bail!("cannot resume: unhealthy nodes remain");
        }
        self.status = if self.all_pinned_healthy() {
            RolloutStatus::Done
        } else {
            RolloutStatus::Running
        };
        Ok(())
    }

    /// Per-node rollback state for operator inspection.
    #[must_use]
    pub fn rollback_state(&self) -> BTreeMap<String, Option<String>> {
        self.nodes
            .iter()
            .map(|(id, n)| (id.clone(), n.rollback_to.clone()))
            .collect()
    }

    fn open_wave_done(&self) -> bool {
        self.nodes
            .values()
            .filter(|n| n.wave <= self.open_wave)
            .all(|n| n.healthy && n.current.as_deref() == Some(&self.revision))
    }

    fn all_pinned_healthy(&self) -> bool {
        self.nodes
            .values()
            .all(|n| n.healthy && n.current.as_deref() == Some(&self.revision))
    }
}
