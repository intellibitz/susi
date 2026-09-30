//! Verifier-driven escalation from a real did-it-work signal.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierEscalation {
    pub escalate: bool,
    pub next_tier: String,
}

/// Escalate to a stronger model when the verifier says the step failed.
#[must_use]
pub fn from_verifier(step_ok: bool, current_tier: &str) -> VerifierEscalation {
    if step_ok {
        return VerifierEscalation {
            escalate: false,
            next_tier: current_tier.into(),
        };
    }
    let next = match current_tier {
        "local" => "cheap_cloud",
        "cheap_cloud" => "frontier",
        _ => "frontier",
    };
    VerifierEscalation {
        escalate: true,
        next_tier: next.into(),
    }
}

#[cfg(test)]
mod verifier_escalation_tests {
    use super::*;

    #[test]
    fn verifier_escalation_steps_up_on_failure() {
        assert!(!from_verifier(true, "local").escalate);
        let e = from_verifier(false, "local");
        assert!(e.escalate);
        assert_eq!(e.next_tier, "cheap_cloud");
    }
}
