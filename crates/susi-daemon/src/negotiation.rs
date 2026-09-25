//! Peer task negotiation (Swarm OS Bullet 19)
//!
//! Offer, accept, commit, and reject. Commit is only legal after accept.
//! Reject is legal from offered or accepted, and never after commit.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Offered,
    Accepted,
    Committed,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiation {
    pub id: String,
    pub offerer: String,
    pub counterparty: String,
    pub task: String,
    pub phase: Phase,
}

impl Negotiation {
    pub fn offer(id: &str, offerer: &str, counterparty: &str, task: &str) -> Self {
        Self {
            id: id.to_string(),
            offerer: offerer.to_string(),
            counterparty: counterparty.to_string(),
            task: task.to_string(),
            phase: Phase::Offered,
        }
    }

    pub fn accept(&mut self) -> Result<(), &'static str> {
        match self.phase {
            Phase::Offered => {
                self.phase = Phase::Accepted;
                Ok(())
            }
            Phase::Accepted | Phase::Committed | Phase::Rejected => Err("accept requires offered"),
        }
    }

    pub fn commit(&mut self) -> Result<(), &'static str> {
        match self.phase {
            Phase::Accepted => {
                self.phase = Phase::Committed;
                Ok(())
            }
            Phase::Offered | Phase::Committed | Phase::Rejected => Err("commit requires accepted"),
        }
    }

    pub fn reject(&mut self) -> Result<(), &'static str> {
        match self.phase {
            Phase::Offered | Phase::Accepted => {
                self.phase = Phase::Rejected;
                Ok(())
            }
            Phase::Committed | Phase::Rejected => Err("committed negotiations cannot be rejected"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_follows_accept_and_then_reject_is_refused() {
        let mut deal = Negotiation::offer("n1", "a", "b", "review patch");
        assert!(deal.commit().is_err());
        deal.accept().unwrap();
        deal.commit().unwrap();
        assert_eq!(deal.phase, Phase::Committed);
        assert!(deal.reject().is_err());
    }

    #[test]
    fn an_offer_can_be_rejected_before_commit() {
        let mut deal = Negotiation::offer("n2", "a", "b", "review patch");
        deal.reject().unwrap();
        assert_eq!(deal.phase, Phase::Rejected);
        assert!(deal.accept().is_err());
    }
}
