//! Canaries that must fail, proving the scorer can detect failure (T-DEEPSEEK-202).
//!
//! This integration test exercises `susi_gawd::dev_canary::evaluate_canary`
//! with a trial whose candidate metric regresses beyond the declared limit,
//! confirming that the scorer returns `StopTrial`.

use susi_gawd::dev_canary::{evaluate_canary, CanaryResult, CanaryTrial};

#[test]
fn intent_ladder_canaries() {
    // candidate_metric (0.5) is worse than baseline_metric (1.0) by 0.5,
    // which exceeds regression_limit (0.4) — the scorer must stop.
    let trial = CanaryTrial {
        candidate_metric: 0.5,
        baseline_metric: 1.0,
        regression_limit: 0.4,
    };
    assert_eq!(evaluate_canary(&trial), CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_nan_candidate() {
    let trial = CanaryTrial {
        candidate_metric: f64::NAN,
        baseline_metric: 1.0,
        regression_limit: 0.4,
    };
    assert_eq!(evaluate_canary(&trial), CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_nan_baseline() {
    let trial = CanaryTrial {
        candidate_metric: 0.9,
        baseline_metric: f64::NAN,
        regression_limit: 0.4,
    };
    assert_eq!(evaluate_canary(&trial), CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_negative_limit() {
    let trial = CanaryTrial {
        candidate_metric: 0.9,
        baseline_metric: 1.0,
        regression_limit: -0.1,
    };
    assert_eq!(evaluate_canary(&trial), CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_within_limit_continues() {
    // Control scenario: candidate remains within acceptable degradation threshold.
    // delta = 1.0 - 0.9 = 0.1, which is within regression_limit (0.4)
    let trial = CanaryTrial {
        candidate_metric: 0.9,
        baseline_metric: 1.0,
        regression_limit: 0.4,
    };
    assert_eq!(evaluate_canary(&trial), CanaryResult::Continue);
}
