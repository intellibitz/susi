//! Distributed Consensus: Term-Based Leader Election (Swarm OS Bullet 53)
//!
//! A single node's view of Raft's leader-election safety rule: grant at
//! most one vote per term, only to a candidate whose term is at least as
//! current as this node's, and step down as leader the moment a newer
//! term is observed. `SusiSupervisor::elect_leader` (`gawd/amas.rs`)
//! builds the swarm-wide bully election on top of this same term-gating
//! primitive.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub struct RaftNode {
    id: String,
    term: AtomicU64,
    voted_for: RwLock<Option<(u64, String)>>,
    leader: AtomicBool,
}

impl RaftNode {
    pub fn new(id: impl Into<String>, leader: bool) -> Self {
        Self {
            id: id.into(),
            term: AtomicU64::new(0),
            voted_for: RwLock::new(None),
            leader: AtomicBool::new(leader),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_leader(&self) -> bool {
        self.leader.load(Ordering::Acquire)
    }

    pub fn current_term(&self) -> u64 {
        self.term.load(Ordering::Acquire)
    }

    /// Decides whether to grant `candidate_id` a vote for `candidate_term`,
    /// mirroring Raft's `RequestVote` RPC safety rule.
    pub fn request_vote(&self, candidate_id: &str, candidate_term: u64) -> bool {
        if candidate_term < self.term.load(Ordering::Acquire) {
            return false;
        }
        if candidate_term > self.term.load(Ordering::Acquire) {
            self.term.store(candidate_term, Ordering::Release);
            self.leader.store(false, Ordering::Release);
            *self.voted_for.write().unwrap_or_else(|e| e.into_inner()) = None;
        }
        let mut voted_for = self.voted_for.write().unwrap_or_else(|e| e.into_inner());
        match voted_for.as_ref() {
            Some((term, voted_id)) if *term == candidate_term && voted_id != candidate_id => false,
            _ => {
                *voted_for = Some((candidate_term, candidate_id.to_string()));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_at_most_one_vote_per_term() {
        let node = RaftNode::new("follower-1", false);
        assert!(node.request_vote("candidate-a", 1));
        // Same term, different candidate: already committed to candidate-a.
        assert!(!node.request_vote("candidate-b", 1));
        // Re-requesting the same candidate/term is idempotent.
        assert!(node.request_vote("candidate-a", 1));
    }

    #[test]
    fn newer_term_resets_the_vote_and_steps_down_leader() {
        let node = RaftNode::new("node-1", true);
        assert!(node.is_leader());
        assert!(node.request_vote("candidate-a", 1));
        assert!(!node.is_leader());
        assert_eq!(node.current_term(), 1);

        // A higher term reopens voting even for the same candidate slot.
        assert!(node.request_vote("candidate-b", 2));
        assert_eq!(node.current_term(), 2);
    }

    #[test]
    fn stale_term_requests_are_rejected() {
        let node = RaftNode::new("node-1", false);
        assert!(node.request_vote("candidate-a", 5));
        assert!(!node.request_vote("candidate-b", 3));
    }
}
