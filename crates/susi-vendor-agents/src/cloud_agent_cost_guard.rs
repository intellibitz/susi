//! Cost guard for cloud agents (Devin, Manus).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostGuard {
    pub allow: bool,
    pub reason: String,
}

/// Block a cloud-agent run when projected cost exceeds the remaining budget.
#[must_use]
pub fn guard_cloud_agent(projected_usd: f64, remaining_budget_usd: f64) -> CostGuard {
    if projected_usd <= remaining_budget_usd {
        CostGuard {
            allow: true,
            reason: "within budget".into(),
        }
    } else {
        CostGuard {
            allow: false,
            reason: format!(
                "projected ${projected_usd:.2} exceeds remaining ${remaining_budget_usd:.2}"
            ),
        }
    }
}

#[cfg(test)]
mod cloud_agent_cost_guard_tests {
    use super::*;

    #[test]
    fn cloud_agent_cost_guard_blocks_over_budget() {
        assert!(guard_cloud_agent(1.0, 5.0).allow);
        let deny = guard_cloud_agent(10.0, 2.0);
        assert!(!deny.allow);
        assert!(deny.reason.contains("exceeds"));
    }
}
