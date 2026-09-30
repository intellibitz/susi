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
