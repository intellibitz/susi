//! Candidate canaries on the dev instance (VC-201-017).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanaryTrial {
    pub candidate_metric: f64,
    pub baseline_metric: f64,
    pub regression_limit: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanaryResult {
    Continue,
    StopTrial,
}

/// Crossing a declared regression limit stops the trial without touching release.
#[must_use]
pub fn evaluate_canary(t: &CanaryTrial) -> CanaryResult {
    let delta = t.baseline_metric - t.candidate_metric;
    if delta > t.regression_limit {
        CanaryResult::StopTrial
    } else {
        CanaryResult::Continue
    }
}

#[must_use]
pub fn dev_instance_ports(base: u16) -> [u16; 5] {
    [base, base + 1, base + 2, base + 3, base + 4]
}
