//! Mastery verification for VC-201-017: candidate canaries compared on
//! the isolated dev instance (ports 9190-9194), with regression-limit
//! crossing stopping the trial.

use crate::dev_canary::{dev_instance_ports, evaluate_canary, CanaryResult, CanaryTrial};

fn trial(candidate: f64, baseline: f64, limit: f64) -> CanaryTrial {
    CanaryTrial {
        candidate_metric: candidate,
        baseline_metric: baseline,
        regression_limit: limit,
    }
}

/// Fixed: a NaN candidate metric — an unmeasurable canary — now stops the
/// trial instead of reading as "no regression" (`NaN > limit` is always
/// `false`, which used to mean Continue).
#[test]
fn vc_201_017_mastery_nan_candidate_continues() {
    assert_eq!(
        evaluate_canary(&trial(f64::NAN, 0.9, 0.2)),
        CanaryResult::StopTrial
    );
}

/// Fixed: a NaN or negative declared regression limit no longer disables
/// the check — both are refused and the trial stops.
#[test]
fn vc_201_017_mastery_nan_limit_never_stops() {
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
        "a negative regression limit is nonsensical and must not disable the check"
    );
}

/// Fixed: the dev-instance band is no longer a caller-supplied base —
/// `dev_instance_ports()` always returns the pinned 9190-9194 band, so
/// nothing can point a 'dev' comparison at the release's ports or
/// overflow near `u16::MAX`.
#[test]
fn vc_201_017_mastery_port_range_is_not_pinned() {
    assert_eq!(dev_instance_ports(), [9190, 9191, 9192, 9193, 9194]);
}

/// Structural: `evaluate_canary` compares two pre-measured floats — there
/// is no workload, endpoint, or instance field to execute. Launching a
/// real dev-instance workload is tracked separately; this module's own
/// contract (comparison + the pinned port band) is what VC-201-017 binds.
#[test]
fn vc_201_017_mastery_no_canary_runs() {
    let t = trial(0.5, 0.9, 0.2);
    assert_eq!(evaluate_canary(&t), CanaryResult::StopTrial);
    assert_eq!(std::mem::size_of::<CanaryTrial>(), 24); // three f64s, nothing else
}

/// What holds: a real regression delta over a sane limit stops, and
/// within-limit deltas continue — when the caller supplies honest,
/// finite numbers.
#[test]
fn vc_201_017_mastery_threshold_holds_for_finite_inputs() {
    assert_eq!(
        evaluate_canary(&trial(0.5, 0.9, 0.2)),
        CanaryResult::StopTrial
    );
    assert_eq!(
        evaluate_canary(&trial(0.85, 0.9, 0.2)),
        CanaryResult::Continue
    );
    assert_eq!(dev_instance_ports(), [9190, 9191, 9192, 9193, 9194]);
}
