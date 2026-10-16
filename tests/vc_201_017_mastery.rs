#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-017: candidate canaries compared on the isolated dev instance
//! (ports 9190-9194), with regression-limit crossing stopping the trial.

use susi_gawd::dev_canary::{dev_instance_ports, evaluate_canary, CanaryResult, CanaryTrial};

fn trial(candidate: f64, baseline: f64, limit: f64) -> CanaryTrial {
    CanaryTrial {
        candidate_metric: candidate,
        baseline_metric: baseline,
        regression_limit: limit,
    }
}

#[test]
fn vc_201_017_mastery() {
    // 1. Pinned dev instance ports: always 9190-9194
    assert_eq!(dev_instance_ports(), [9190, 9191, 9192, 9193, 9194]);

    // 2. NaN candidate metric stops the trial
    assert_eq!(
        evaluate_canary(&trial(f64::NAN, 0.9, 0.2)),
        CanaryResult::StopTrial
    );

    // 3. NaN or negative regression limits stop the trial
    assert_eq!(
        evaluate_canary(&trial(0.0, 1.0, f64::NAN)),
        CanaryResult::StopTrial
    );
    assert_eq!(
        evaluate_canary(&trial(f64::NEG_INFINITY, 1.0, f64::NAN)),
        CanaryResult::StopTrial
    );
    assert_eq!(
        evaluate_canary(&trial(0.5, 1.0, -0.1)),
        CanaryResult::StopTrial,
        "a negative regression limit must not disable the check"
    );

    // 4. Exceeding regression threshold stops the trial
    assert_eq!(
        evaluate_canary(&trial(0.5, 0.9, 0.2)),
        CanaryResult::StopTrial
    );

    // 5. Acceptable delta within limit continues
    assert_eq!(
        evaluate_canary(&trial(0.85, 0.9, 0.2)),
        CanaryResult::Continue
    );

    // 6. Memory layout contract
    assert_eq!(std::mem::size_of::<CanaryTrial>(), 24);
}
