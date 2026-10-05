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

/// Crossing a declared regression limit stops the trial without touching
/// release. An unmeasurable canary — a non-finite metric — or a
/// nonsensical (non-finite or negative) declared limit must not be read
/// as "no regression": `NaN > limit` and `x > NaN` are both `false` under
/// IEEE 754, which let an unmeasured run or a disabled limit sail through
/// as `Continue`. Both now stop the trial instead.
#[must_use]
pub fn evaluate_canary(t: &CanaryTrial) -> CanaryResult {
    if !t.candidate_metric.is_finite()
        || !t.baseline_metric.is_finite()
        || !t.regression_limit.is_finite()
        || t.regression_limit < 0.0
    {
        return CanaryResult::StopTrial;
    }
    let delta = t.baseline_metric - t.candidate_metric;
    if delta > t.regression_limit {
        CanaryResult::StopTrial
    } else {
        CanaryResult::Continue
    }
}

/// The dev instance's pinned isolation band — never configurable by a
/// caller, so a candidate comparison can never be pointed at the
/// installed release's ports or overflow near `u16::MAX`.
pub const DEV_INSTANCE_BASE_PORT: u16 = 9190;

#[must_use]
pub fn dev_instance_ports() -> [u16; 5] {
    let base = DEV_INSTANCE_BASE_PORT;
    [base, base + 1, base + 2, base + 3, base + 4]
}
