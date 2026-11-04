//! Select complementary swarm roles from evidence (VC-201-027).
//!
//! Choose implementer, verifier, and specialist roles using measured task
//! suitability and granted capabilities; report when multi-agent is cheaper.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvidence {
    pub id: String,
    pub capabilities: BTreeSet<String>,
    /// Suitability score for the task class in \[0, 1\].
    pub suitability: f64,
    /// Mean USD cost per successful outcome.
    pub cost_per_success: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub implementer: String,
    pub verifier: String,
    pub specialist: Option<String>,
    pub multi_agent_cheaper: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectError {
    NoImplementer,
    NoIndependentVerifier,
}

/// Pick complementary roles: verifier ≠ implementer; specialist optional when
/// a capability gap remains. Multi-agent is reported cheaper when the sum of
/// role costs is below a single-agent solo cost.
pub fn select_roles(
    agents: &[AgentEvidence],
    required_caps: &BTreeSet<String>,
    solo_cost: f64,
) -> Result<RoleAssignment, SelectError> {
    let mut ranked: Vec<_> = agents.iter().filter(|a| a.suitability > 0.0).collect();
    ranked.sort_by(|a, b| {
        b.suitability
            .partial_cmp(&a.suitability)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let implementer = ranked.first().ok_or(SelectError::NoImplementer)?;
    let verifier = ranked
        .iter()
        .find(|a| a.id != implementer.id)
        .ok_or(SelectError::NoIndependentVerifier)?;

    let covered: BTreeSet<_> = implementer
        .capabilities
        .union(&verifier.capabilities)
        .cloned()
        .collect();
    let gap: BTreeSet<_> = required_caps.difference(&covered).cloned().collect();
    let specialist = if gap.is_empty() {
        None
    } else {
        ranked
            .iter()
            .find(|a| {
                a.id != implementer.id
                    && a.id != verifier.id
                    && gap.iter().any(|c| a.capabilities.contains(c))
            })
            .map(|a| a.id.clone())
    };

    let multi_cost = implementer.cost_per_success
        + verifier.cost_per_success
        + specialist
            .as_ref()
            .and_then(|sid| agents.iter().find(|a| a.id == *sid))
            .map(|a| a.cost_per_success)
            .unwrap_or(0.0);

    Ok(RoleAssignment {
        implementer: implementer.id.clone(),
        verifier: verifier.id.clone(),
        specialist,
        multi_agent_cheaper: multi_cost < solo_cost,
    })
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn agent(id: &str, caps: &[&str], suitability: f64, cost: f64) -> AgentEvidence {
        AgentEvidence {
            id: id.into(),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            suitability,
            cost_per_success: cost,
        }
    }

    #[test]
    fn select_roles_returns_no_implementer_when_no_agent_has_positive_suitability() {
        let required = BTreeSet::new();
        assert_eq!(
            select_roles(&[], &required, 1.0),
            Err(SelectError::NoImplementer)
        );
        let zero = vec![agent("a1", &["rust"], 0.0, 0.1)];
        assert_eq!(
            select_roles(&zero, &required, 1.0),
            Err(SelectError::NoImplementer)
        );
    }

    #[test]
    fn select_roles_returns_no_independent_verifier_when_only_one_agent_qualifies() {
        let required = BTreeSet::from(["rust".to_string()]);
        let agents = vec![
            agent("solo", &["rust"], 0.9, 0.2),
            agent("unfit", &["rust"], 0.0, 0.1),
        ];
        assert_eq!(
            select_roles(&agents, &required, 1.0),
            Err(SelectError::NoIndependentVerifier)
        );
    }

    #[test]
    fn select_roles_assigns_highest_suitability_implementer_and_independent_verifier() {
        let required = BTreeSet::from(["rust".to_string(), "test".to_string()]);
        let agents = vec![
            agent("ver", &["test"], 0.75, 0.10),
            agent("imp", &["rust"], 0.95, 0.15),
        ];
        let assigned = select_roles(&agents, &required, 0.50).unwrap();
        assert_eq!(assigned.implementer, "imp");
        assert_eq!(assigned.verifier, "ver");
        assert_eq!(assigned.specialist, None);
        assert!(assigned.multi_agent_cheaper);
    }

    #[test]
    fn select_roles_picks_specialist_for_uncovered_capability_and_checks_cost_vs_solo() {
        let required =
            BTreeSet::from(["rust".to_string(), "audit".to_string(), "wasm".to_string()]);
        let agents = vec![
            agent("imp", &["rust"], 0.9, 0.40),
            agent("ver", &["audit"], 0.8, 0.40),
            agent("spec", &["wasm"], 0.6, 0.30),
        ];
        let assigned = select_roles(&agents, &required, 1.00).unwrap();
        assert_eq!(assigned.specialist.as_deref(), Some("spec"));
        assert!(!assigned.multi_agent_cheaper);
    }
}
