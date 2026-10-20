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
