//! Auto-update on by default with health-gated rollback.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePolicy {
    pub auto_update: bool,
    pub health_gate: bool,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            auto_update: true,
            health_gate: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    Apply,
    Rollback,
    Skip,
}

#[must_use]
pub fn decide_update(policy: &UpdatePolicy, candidate_healthy: bool) -> UpdateDecision {
    if !policy.auto_update {
        return UpdateDecision::Skip;
    }
    if policy.health_gate && !candidate_healthy {
        return UpdateDecision::Rollback;
    }
    UpdateDecision::Apply
}

#[cfg(test)]
mod zc_updates_default_tests {
    use super::*;

    #[test]
    fn zc_updates_default_on_with_health_gate() {
        let p = UpdatePolicy::default();
        assert!(p.auto_update);
        assert_eq!(decide_update(&p, true), UpdateDecision::Apply);
        assert_eq!(decide_update(&p, false), UpdateDecision::Rollback);
    }
}
