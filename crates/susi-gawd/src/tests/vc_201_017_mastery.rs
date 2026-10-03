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

/// Falsification: a NaN candidate metric — an unmeasurable canary —
/// reports Continue, because delta is NaN and NaN > limit is false. The
/// single most important canary failure mode (the run produced no valid
/// measurement) sails through the gate.
#[test]
fn vc_201_017_mastery_nan_candidate_continues() {
    assert_eq!(
        evaluate_canary(&trial(f64::NAN, 0.9, 0.2)),
        CanaryResult::Continue
    );
}

/// Falsification: a NaN or negative 'declared regression limit' disables
/// the declared limit entirely — every regression passes.
#[test]
fn vc_201_017_mastery_nan_limit_never_stops() {
    assert_eq!(
        evaluate_canary(&trial(0.0, 1.0, f64::NAN)),
        CanaryResult::Continue
    );
    // And an infinitely regressed candidate (candidate -inf) still
    // 'continues' under a NaN limit.
    assert_eq!(
        evaluate_canary(&trial(f64::NEG_INFINITY, 1.0, f64::NAN)),
        CanaryResult::Continue
    );
}

/// Falsification: 'the 9190-9194 port range' is not enforced — the
/// helper returns base..base+5 for ANY base, so callers can allocate
/// dev 'isolation' onto ports colliding with the installed release or
/// anywhere else; and a base near u16::MAX panics on overflow.
#[test]
fn vc_201_017_mastery_port_range_is_not_pinned() {
    // Arbitrary bases — including release-range-adjacent — are accepted.
    assert_eq!(dev_instance_ports(9000)[0], 9000);
    assert_eq!(dev_instance_ports(1)[0], 1);
    // And a base of 65532+ overflows u16: the range is arithmetic, not
    // the pinned 9190-9194 band the claim names.
    assert!(std::panic::catch_unwind(|| dev_instance_ports(65533)).is_err());
}

/// Falsification: nothing measures the canary. evaluate_canary compares
/// two caller-supplied floats — no dev instance is started, no workload
/// runs, no metric is observed. 'Compare a candidate on ~/.susi-dev
/// using an isolated evaluation workload' is a data bag comparison.
/// (Structural: CanaryTrial carries only the two floats and the limit —
/// there is no workload, endpoint, or instance field to execute.)
#[test]
fn vc_201_017_mastery_no_canary_runs() {
    // There is nothing to call that would launch a trial: the only
    // entry point takes pre-measured numbers.
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
    assert_eq!(dev_instance_ports(9190), [9190, 9191, 9192, 9193, 9194]);
}
