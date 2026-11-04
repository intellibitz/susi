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

    #[test]
    fn zc_agent_pick_returns_none_when_empty_or_none_ready() {
        assert_eq!(pick_agent(&[]), None);
        let unready = [
            AgentCandidate {
                id: "x".into(),
                ready: false,
                score: 100,
            },
            AgentCandidate {
                id: "y".into(),
                ready: false,
                score: 200,
            },
        ];
        assert_eq!(pick_agent(&unready), None);
    }

    #[test]
    fn zc_agent_pick_selects_zero_score_ready_candidate_when_higher_scores_unready() {
        let cands = [
            AgentCandidate {
                id: "offline-high".into(),
                ready: false,
                score: u32::MAX,
            },
            AgentCandidate {
                id: "ready-zero".into(),
                ready: true,
                score: 0,
            },
        ];
        assert_eq!(pick_agent(&cands).as_deref(), Some("ready-zero"));
    }

    #[test]
    fn agent_candidate_serde_json_roundtrip_and_traits() {
        let cand = AgentCandidate {
            id: "agent-7".into(),
            ready: true,
            score: 42,
        };
        let encoded = serde_json::to_string(&cand).unwrap();
        let decoded: AgentCandidate = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, cand.clone());
        assert!(format!("{cand:?}").contains("agent-7"));
    }
}
