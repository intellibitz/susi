//! CI change gate (Swarm OS Bullet 46)
//!
//! A proposal may merge only after its quorum is satisfied. This is the
//! kernel gate in front of a pipeline; it does not itself push to a forge.

use crate::approval::Quorum;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeProposal {
    pub id: String,
    pub summary: String,
    pub quorum: Quorum,
}

impl ChangeProposal {
    pub fn new(id: &str, summary: &str, required_approvals: usize) -> Self {
        Self {
            id: id.to_string(),
            summary: summary.to_string(),
            quorum: Quorum::new(required_approvals),
        }
    }

    pub fn approve(&mut self, cell_id: &str) -> bool {
        self.quorum.vote(cell_id)
    }

    pub fn may_merge(&self) -> bool {
        self.quorum.satisfied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_stays_closed_until_the_quorum_is_met() {
        let mut proposal = ChangeProposal::new("pr-1", "fix auth", 2);
        assert!(!proposal.may_merge());
        proposal.approve("reviewer-a");
        assert!(!proposal.may_merge());
        proposal.approve("reviewer-b");
        assert!(proposal.may_merge());
    }
}
