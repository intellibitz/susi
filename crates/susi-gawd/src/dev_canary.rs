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

/// One dimension's canary trial, oriented so that higher is always
/// better: a lower-is-better dimension (latency) is compared on its
/// negated value, so `baseline - candidate > 0` always means the
/// candidate regressed past the predeclared bound.
fn dimension_trial(
    d: &crate::scorecard::DimensionScore,
    spec: &crate::scorecard::DimensionSpec,
) -> CanaryTrial {
    let sign = if spec.lower_is_better { -1.0 } else { 1.0 };
    CanaryTrial {
        candidate_metric: sign * d.value,
        baseline_metric: sign * spec.threshold,
        regression_limit: 0.0,
    }
}

/// The production canary gate on the evaluator's measured scorecard:
/// every dimension's raw measured value is compared against its
/// predeclared bound with zero regression tolerance — quality must not
/// fall below `spec.quality.threshold`, latency must not exceed
/// `spec.latency.threshold`, and likewise for reliability, resource and
/// operator effort. Unlike `summary_ok`, this ignores the reported
/// uncertainty, so a negative or NaN uncertainty cannot widen a
/// below-bound value into a pass. Any non-finite value stops the trial.
///
/// The comparison is bound to the pinned dev-instance isolation band
/// ([`dev_instance_ports`]); a band that ever stopped matching
/// [`DEV_INSTANCE_BASE_PORT`] stops the trial rather than comparing on
/// the installed release's ports.
#[must_use]
pub fn canary_gate_from_scorecard(
    measured: &crate::scorecard::ImprovementScorecard,
    spec: &crate::scorecard::ScorecardSpec,
) -> CanaryResult {
    let ports = dev_instance_ports();
    let pinned = ports
        .iter()
        .zip(0u16..)
        .all(|(p, i)| DEV_INSTANCE_BASE_PORT.checked_add(i) == Some(*p));
    if !pinned {
        return CanaryResult::StopTrial;
    }
    let trials = [
        dimension_trial(&measured.quality, &spec.quality),
        dimension_trial(&measured.latency, &spec.latency),
        dimension_trial(&measured.reliability, &spec.reliability),
        dimension_trial(&measured.resource, &spec.resource),
        dimension_trial(&measured.operator_effort, &spec.operator_effort),
    ];
    if trials
        .iter()
        .any(|t| evaluate_canary(t) == CanaryResult::StopTrial)
    {
        CanaryResult::StopTrial
    } else {
        CanaryResult::Continue
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn evaluate_canary_continues_when_regression_is_within_limit_or_candidate_improves() {
        let improved = CanaryTrial {
            candidate_metric: 0.96,
            baseline_metric: 0.92,
            regression_limit: 0.02,
        };
        assert_eq!(evaluate_canary(&improved), CanaryResult::Continue);

        let within = CanaryTrial {
            candidate_metric: 0.91,
            baseline_metric: 0.92,
            regression_limit: 0.02,
        };
        assert_eq!(evaluate_canary(&within), CanaryResult::Continue);
    }

    #[test]
    fn evaluate_canary_stops_trial_when_delta_exceeds_regression_limit() {
        let regressed = CanaryTrial {
            candidate_metric: 0.85,
            baseline_metric: 0.92,
            regression_limit: 0.02,
        };
        assert_eq!(evaluate_canary(&regressed), CanaryResult::StopTrial);
    }

    #[test]
    fn evaluate_canary_stops_trial_on_non_finite_metrics_or_negative_limit() {
        let nan_candidate = CanaryTrial {
            candidate_metric: f64::NAN,
            baseline_metric: 0.90,
            regression_limit: 0.05,
        };
        assert_eq!(evaluate_canary(&nan_candidate), CanaryResult::StopTrial);

        let neg_limit = CanaryTrial {
            candidate_metric: 0.95,
            baseline_metric: 0.90,
            regression_limit: -0.01,
        };
        assert_eq!(evaluate_canary(&neg_limit), CanaryResult::StopTrial);
    }

    #[test]
    fn dev_instance_ports_returns_five_consecutive_ports_from_pinned_base() {
        let ports = dev_instance_ports();
        assert_eq!(ports, [9190, 9191, 9192, 9193, 9194]);
        assert_eq!(ports[0], DEV_INSTANCE_BASE_PORT);
    }

    #[test]
    fn canary_trial_and_result_serde_roundtrip_and_exact_limit_boundary_continues() {
        let boundary = CanaryTrial {
            candidate_metric: 0.90,
            baseline_metric: 0.95,
            regression_limit: 0.05,
        };
        let res = evaluate_canary(&boundary);
        assert_eq!(res, CanaryResult::Continue);

        let trial_json = serde_json::to_string(&boundary).expect("serialize CanaryTrial");
        let trial_back: CanaryTrial =
            serde_json::from_str(&trial_json).expect("deserialize CanaryTrial");
        assert_eq!(trial_back, boundary);

        let res_json = serde_json::to_string(&res).expect("serialize CanaryResult");
        let res_back: CanaryResult =
            serde_json::from_str(&res_json).expect("deserialize CanaryResult");
        assert_eq!(res_back, res);
    }
}
