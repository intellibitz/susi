//! Per-mission sandbox with CPU, memory and time limits.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionLimits {
    pub cpu_pct: u32,
    pub memory_mb: u32,
    pub timeout_secs: u64,
}

/// Derive mission sandbox limits from risk class.
#[must_use]
pub fn limits_for_risk(risk: &str) -> MissionLimits {
    match risk {
        "high" => MissionLimits {
            cpu_pct: 50,
            memory_mb: 512,
            timeout_secs: 30,
        },
        "medium" => MissionLimits {
            cpu_pct: 100,
            memory_mb: 1024,
            timeout_secs: 120,
        },
        _ => MissionLimits {
            cpu_pct: 200,
            memory_mb: 2048,
            timeout_secs: 600,
        },
    }
}

#[cfg(test)]
mod mission_sandbox_limits_tests {
    use super::*;

    #[test]
    fn mission_sandbox_limits_tighten_for_high_risk() {
        let h = limits_for_risk("high");
        let l = limits_for_risk("low");
        assert!(h.memory_mb < l.memory_mb);
        assert!(h.timeout_secs < l.timeout_secs);
    }
}
