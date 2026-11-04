//! Test ensuring a failing canary is detected by the scorer.

use crate::dev_canary::{evaluate_canary, CanaryResult, CanaryTrial};

#[test]
fn intent_ladder_canaries() {
    // This trial is constructed so that the candidate metric is worse than the baseline
    // by more than the allowed regression limit, which should cause the scorer to stop.
    let trial = CanaryTrial {
        candidate_metric: 0.5,
        baseline_metric: 1.0,
        regression_limit: 0.4,
    };
    let result = evaluate_canary(&trial);
    assert_eq!(result, CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_equal_or_better_metric_continues() {
    let equal = CanaryTrial {
        candidate_metric: 1.0,
        baseline_metric: 1.0,
        regression_limit: 0.1,
    };
    assert_eq!(evaluate_canary(&equal), CanaryResult::Continue);

    let improved = CanaryTrial {
        candidate_metric: 1.25,
        baseline_metric: 1.0,
        regression_limit: 0.1,
    };
    assert_eq!(evaluate_canary(&improved), CanaryResult::Continue);
}

#[test]
fn intent_ladder_canaries_drop_within_regression_limit_continues() {
    let within = CanaryTrial {
        candidate_metric: 0.7,
        baseline_metric: 1.0,
        regression_limit: 0.4,
    };
    assert_eq!(evaluate_canary(&within), CanaryResult::Continue);
}

#[test]
fn intent_ladder_canaries_exact_boundary_and_invalid_inputs() {
    let exact = CanaryTrial {
        candidate_metric: 0.75,
        baseline_metric: 1.0,
        regression_limit: 0.25,
    };
    assert_eq!(evaluate_canary(&exact), CanaryResult::Continue);

    let negative_limit = CanaryTrial {
        candidate_metric: 1.0,
        baseline_metric: 1.0,
        regression_limit: -0.01,
    };
    assert_eq!(evaluate_canary(&negative_limit), CanaryResult::StopTrial);

    let nan_metric = CanaryTrial {
        candidate_metric: f64::NAN,
        baseline_metric: 1.0,
        regression_limit: 0.5,
    };
    assert_eq!(evaluate_canary(&nan_metric), CanaryResult::StopTrial);
}

#[test]
fn intent_ladder_canaries_nan_baseline_or_nan_regression_limit_stops_trial() {
    let nan_baseline = CanaryTrial {
        candidate_metric: 1.0,
        baseline_metric: f64::NAN,
        regression_limit: 0.5,
    };
    assert_eq!(evaluate_canary(&nan_baseline), CanaryResult::StopTrial);

    let nan_limit = CanaryTrial {
        candidate_metric: 1.0,
        baseline_metric: 1.0,
        regression_limit: f64::NAN,
    };
    assert_eq!(evaluate_canary(&nan_limit), CanaryResult::StopTrial);
}
