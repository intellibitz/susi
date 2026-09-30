//! Resource governor for background provisioning.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernorDecision {
    pub allow: bool,
    pub reason: String,
}

/// Gate a background provision when CPU/mem pressure is high.
#[must_use]
pub fn govern(cpu_pct: f64, mem_pct: f64, is_foreground: bool) -> GovernorDecision {
    if is_foreground {
        return GovernorDecision {
            allow: true,
            reason: "foreground always allowed".into(),
        };
    }
    if cpu_pct > 85.0 || mem_pct > 90.0 {
        return GovernorDecision {
            allow: false,
            reason: "defer under resource pressure".into(),
        };
    }
    GovernorDecision {
        allow: true,
        reason: "within budget".into(),
    }
}

#[cfg(test)]
mod resource_governor_tests {
    use super::*;

    #[test]
    fn resource_governor_defers_under_pressure() {
        assert!(govern(10.0, 10.0, false).allow);
        assert!(!govern(90.0, 50.0, false).allow);
        assert!(govern(99.0, 99.0, true).allow);
    }
}
