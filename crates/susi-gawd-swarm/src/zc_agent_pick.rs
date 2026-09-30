//! Pick an agent for a task from readiness.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCandidate {
    pub id: String,
    pub ready: bool,
    pub score: u32,
}

#[must_use]
pub fn pick_agent(cands: &[AgentCandidate]) -> Option<String> {
    cands
        .iter()
        .filter(|c| c.ready)
        .max_by_key(|c| c.score)
        .map(|c| c.id.clone())
}

#[cfg(test)]
mod zc_agent_pick_tests {
    use super::*;

    #[test]
    fn zc_agent_pick_highest_ready_score() {
        let pick = pick_agent(&[
            AgentCandidate {
                id: "a".into(),
                ready: true,
                score: 1,
            },
            AgentCandidate {
                id: "b".into(),
                ready: false,
                score: 99,
            },
            AgentCandidate {
                id: "c".into(),
                ready: true,
                score: 5,
            },
        ]);
        assert_eq!(pick.as_deref(), Some("c"));
    }
}
