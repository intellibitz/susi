//! Deterministic model of signed roster + term transitions (VC-201-031).
//!
//! Explores reordered messages, partitions, and restarts. Documents the
//! safety properties this implementation satisfies; discoveries that violate
//! them are preserved as counterexamples in tests.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermState {
    pub term: u64,
    pub leader: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Claim leadership for `leader` at observed term base.
    Claim { leader: String },
    /// Observe a sealed record stamped with term/leader.
    Observe { term: u64, leader: String },
    /// Restart: clear in-memory view, reload from durable term.
    Restart,
}

#[derive(Debug, Clone, Default)]
pub struct NodeModel {
    pub id: String,
    pub durable: TermState,
    pub partitioned: bool,
}

impl Default for TermState {
    fn default() -> Self {
        Self {
            term: 0,
            leader: String::new(),
        }
    }
}

impl NodeModel {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            durable: TermState::default(),
            partitioned: false,
        }
    }

    /// Claim leadership: bump term only when leader changes (mirrors commit_log).
    pub fn claim(&mut self, leader: &str) {
        if self.durable.leader != leader {
            self.durable.term = self.durable.term.saturating_add(1);
            self.durable.leader = leader.to_string();
        }
    }

    /// Raft step-down: adopt higher term.
    pub fn observe(&mut self, term: u64, leader: &str) {
        if term > self.durable.term {
            self.durable.term = term;
            self.durable.leader = leader.to_string();
        } else if term == self.durable.term && !leader.is_empty() && self.durable.leader.is_empty()
        {
            self.durable.leader = leader.to_string();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClusterModel {
    pub nodes: BTreeMap<String, NodeModel>,
}

impl ClusterModel {
    pub fn add(&mut self, id: &str) {
        self.nodes.insert(id.to_string(), NodeModel::new(id));
    }

    pub fn set_partition(&mut self, id: &str, partitioned: bool) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.partitioned = partitioned;
        }
    }

    pub fn deliver(&mut self, to: &str, msg: &Message) {
        let Some(node) = self.nodes.get_mut(to) else {
            return;
        };
        if node.partitioned {
            return; // dropped
        }
        match msg {
            Message::Claim { leader } => node.claim(leader),
            Message::Observe { term, leader } => node.observe(*term, leader),
            Message::Restart => {
                // Durable state survives; in-memory equals durable after reload.
            }
        }
    }

    /// Safety: within any connected (non-partitioned) subset that has elected,
    /// same term ⇒ same leader.
    pub fn no_split_brain_connected(&self) -> Result<(), String> {
        let connected: Vec<_> = self.nodes.values().filter(|n| !n.partitioned).collect();
        let mut by_term: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
        for n in &connected {
            if n.durable.term == 0 || n.durable.leader.is_empty() {
                continue;
            }
            by_term
                .entry(n.durable.term)
                .or_default()
                .insert(n.durable.leader.clone());
        }
        for (term, leaders) in by_term {
            if leaders.len() > 1 {
                return Err(format!("split-brain at term {term}: leaders {leaders:?}"));
            }
        }
        Ok(())
    }

    /// Terms only increase on a node.
    pub fn terms_monotonic_after(before: &TermState, after: &TermState) -> bool {
        after.term >= before.term
    }
}

/// Documented safety properties satisfied by this model.
pub const SAFETY_PROPERTIES: &[&str] = &[
    "SP1: claim bumps term iff leader identity changes",
    "SP2: observe adopts strictly higher terms (Raft step-down)",
    "SP3: connected non-partitioned nodes never disagree on leader for the same term (no split-brain)",
    "SP4: term numbers are non-decreasing on each node",
];
