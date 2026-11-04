//! Deterministic network chaos model for election / quorum commit (VC-201-095).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecord {
    pub term: u64,
    pub leader: String,
    pub seq: u64,
    pub payload: String,
}

#[derive(Debug, Clone)]
pub struct ChaosNode {
    pub id: String,
    pub term: u64,
    pub leader: String,
    pub log: Vec<CommitRecord>,
    pub inbox: VecDeque<CommitRecord>,
    pub partitioned: bool,
}

impl ChaosNode {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            term: 0,
            leader: String::new(),
            log: Vec::new(),
            inbox: VecDeque::new(),
            partitioned: false,
        }
    }

    pub fn elect(&mut self, candidates: &[(String, f32)]) {
        // Bully: highest trust, then lex id — mirrors amas::elect_leader.
        let winner = candidates
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            })
            .map(|(id, _)| id.clone());
        if let Some(w) = winner {
            if self.leader != w {
                self.term = self.term.saturating_add(1);
                self.leader = w;
            }
        }
    }

    pub fn propose_commit(&self, seq: u64, payload: &str) -> Option<CommitRecord> {
        if self.leader != self.id || self.term == 0 {
            return None;
        }
        Some(CommitRecord {
            term: self.term,
            leader: self.leader.clone(),
            seq,
            payload: payload.to_string(),
        })
    }

    pub fn accept(&mut self, rec: CommitRecord) -> bool {
        if rec.term < self.term {
            return false;
        }
        if rec.term > self.term {
            self.term = rec.term;
            self.leader = rec.leader.clone();
        } else if rec.term == self.term && rec.leader != self.leader && !self.leader.is_empty() {
            // Same term, different leader → reject (split-brain guard).
            return false;
        }
        if self
            .log
            .iter()
            .any(|r| r.seq == rec.seq && r.term == rec.term)
        {
            return true; // duplicate idempotent
        }
        self.log.push(rec);
        true
    }
}

#[derive(Debug, Default)]
pub struct ChaosNetwork {
    pub nodes: BTreeMap<String, ChaosNode>,
    pub delayed: VecDeque<(String, CommitRecord)>,
}

impl ChaosNetwork {
    pub fn add(&mut self, id: &str) {
        self.nodes.insert(id.to_string(), ChaosNode::new(id));
    }

    pub fn partition(&mut self, id: &str, on: bool) {
        if let Some(n) = self.nodes.get_mut(id) {
            n.partitioned = on;
        }
    }

    pub fn broadcast(&mut self, from: &str, rec: CommitRecord, duplicate: bool) {
        let targets: Vec<String> = self.nodes.keys().cloned().collect();
        for t in targets {
            if t == from {
                continue;
            }
            let partitioned = self.nodes.get(&t).is_some_and(|n| n.partitioned)
                || self.nodes.get(from).is_some_and(|n| n.partitioned);
            if partitioned {
                self.delayed.push_back((t, rec.clone()));
                continue;
            }
            if let Some(n) = self.nodes.get_mut(&t) {
                let _ = n.accept(rec.clone());
                if duplicate {
                    let _ = n.accept(rec.clone());
                }
            }
        }
        if let Some(n) = self.nodes.get_mut(from) {
            let _ = n.accept(rec);
        }
    }

    pub fn flush_delayed(&mut self) {
        while let Some((t, rec)) = self.delayed.pop_front() {
            if let Some(n) = self.nodes.get_mut(&t) {
                if !n.partitioned {
                    let _ = n.accept(rec);
                } else {
                    // still partitioned — drop for this model
                }
            }
        }
    }

    /// No two committed records share (term, seq) with different leaders/payloads.
    pub fn assert_no_split_brain_commit(&self) -> Result<(), String> {
        let mut seen: BTreeMap<(u64, u64), (String, String)> = BTreeMap::new();
        for n in self.nodes.values() {
            for r in &n.log {
                let key = (r.term, r.seq);
                if let Some((leader, payload)) = seen.get(&key) {
                    if leader != &r.leader || payload != &r.payload {
                        return Err(format!(
                            "split-brain commit at term={} seq={}: {leader}/{payload} vs {}/{}",
                            r.term, r.seq, r.leader, r.payload
                        ));
                    }
                } else {
                    seen.insert(key, (r.leader.clone(), r.payload.clone()));
                }
            }
        }
        // Connected leaders for same highest term must agree.
        let max_term = self.nodes.values().map(|n| n.term).max().unwrap_or(0);
        let leaders: BTreeSet<_> = self
            .nodes
            .values()
            .filter(|n| !n.partitioned && n.term == max_term && !n.leader.is_empty())
            .map(|n| n.leader.clone())
            .collect();
        if leaders.len() > 1 {
            return Err(format!(
                "connected split leaders at term {max_term}: {leaders:?}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn elect_selects_highest_trust_with_lexicographic_tiebreak_and_advances_term() {
        let mut node = ChaosNode::new("n1");
        let candidates = vec![
            ("n1".to_string(), 0.8),
            ("n2".to_string(), 0.9),
            ("n3".to_string(), 0.9),
        ];
        node.elect(&candidates);
        assert_eq!(node.leader, "n3");
        assert_eq!(node.term, 1);
        node.elect(&candidates);
        assert_eq!(node.term, 1);
    }

    #[test]
    fn propose_commit_and_accept_enforce_leader_term_and_split_brain_guard() {
        let mut leader = ChaosNode::new("n1");
        assert!(leader.propose_commit(1, "init").is_none());
        leader.elect(&[("n1".to_string(), 1.0)]);
        let rec = leader.propose_commit(1, "cmd-a").unwrap();

        let mut follower = ChaosNode::new("n2");
        assert!(follower.accept(rec.clone()));
        assert!(follower.accept(rec));
        assert_eq!(follower.log.len(), 1);

        let split = CommitRecord {
            term: 1,
            leader: "rogue".to_string(),
            seq: 2,
            payload: "cmd-b".to_string(),
        };
        assert!(!follower.accept(split));
    }

    #[test]
    fn broadcast_and_flush_delayed_deliver_after_partition_heals() {
        let mut net = ChaosNetwork::default();
        net.add("n1");
        net.add("n2");
        net.nodes
            .get_mut("n1")
            .unwrap()
            .elect(&[("n1".to_string(), 1.0)]);
        net.partition("n2", true);

        let rec = net
            .nodes
            .get("n1")
            .unwrap()
            .propose_commit(1, "op-1")
            .unwrap();
        net.broadcast("n1", rec, true);
        assert_eq!(net.nodes["n2"].log.len(), 0);
        assert_eq!(net.delayed.len(), 1);

        net.partition("n2", false);
        net.flush_delayed();
        assert_eq!(net.nodes["n2"].log.len(), 1);
        assert!(net.assert_no_split_brain_commit().is_ok());
    }

    #[test]
    fn assert_no_split_brain_commit_detects_conflicting_records_and_connected_leaders() {
        let mut net = ChaosNetwork::default();
        net.add("n1");
        net.add("n2");
        net.nodes.get_mut("n1").unwrap().log.push(CommitRecord {
            term: 2,
            leader: "n1".to_string(),
            seq: 1,
            payload: "a".to_string(),
        });
        net.nodes.get_mut("n2").unwrap().log.push(CommitRecord {
            term: 2,
            leader: "n2".to_string(),
            seq: 1,
            payload: "b".to_string(),
        });
        assert!(net
            .assert_no_split_brain_commit()
            .unwrap_err()
            .contains("split-brain commit"));
    }
}
