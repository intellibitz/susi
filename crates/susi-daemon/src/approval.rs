//! Sensitive-operation quorum (Swarm OS Bullet 53)
//!
//! Deploy, money movement, and data export proceed only after `required`
//! distinct cells have voted. A repeated vote from the same cell does not
//! count twice.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveKind {
    Deploy,
    MoneyMovement,
    DataExport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quorum {
    required: usize,
    voters: HashSet<String>,
}

impl Quorum {
    pub fn new(required: usize) -> Self {
        Self {
            required,
            voters: HashSet::new(),
        }
    }

    /// Records `cell_id`. Returns whether the quorum is now satisfied.
    pub fn vote(&mut self, cell_id: &str) -> bool {
        if self.required == 0 {
            return false;
        }
        self.voters.insert(cell_id.to_string());
        self.satisfied()
    }

    pub fn satisfied(&self) -> bool {
        self.required > 0 && self.voters.len() >= self.required
    }

    pub fn voter_count(&self) -> usize {
        self.voters.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveRequest {
    pub kind: SensitiveKind,
    pub quorum: Quorum,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_distinct_votes_satisfy_a_quorum_of_two() {
        let mut request = SensitiveRequest {
            kind: SensitiveKind::DataExport,
            quorum: Quorum::new(2),
        };
        assert!(!request.quorum.vote("cell-a"));
        assert!(!request.quorum.vote("cell-a"));
        assert_eq!(request.quorum.voter_count(), 1);
        assert!(request.quorum.vote("cell-b"));
        assert!(request.quorum.satisfied());
    }
}
