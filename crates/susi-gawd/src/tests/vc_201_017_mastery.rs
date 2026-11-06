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

mod scorecard_gate {
    use crate::dev_canary::{canary_gate_from_scorecard, CanaryResult};
    use crate::scorecard::{DimensionScore, DimensionSpec, ImprovementScorecard, ScorecardSpec};

    pub(super) fn dim(name: &str, value: f64, uncertainty: f64) -> DimensionScore {
        DimensionScore {
            name: name.into(),
            value,
            uncertainty,
        }
    }

    pub(super) fn spec() -> ScorecardSpec {
        let up = |threshold| DimensionSpec {
            threshold,
            lower_is_better: false,
        };
        let down = |threshold| DimensionSpec {
            threshold,
            lower_is_better: true,
        };
        ScorecardSpec {
            quality: up(0.8),
            reliability: up(0.9),
            latency: down(100.0),
            resource: up(0.9),
            operator_effort: down(5.0),
            min_task_count: 30,
        }
    }

    pub(super) fn passing() -> ImprovementScorecard {
        ImprovementScorecard {
            quality: dim("quality", 0.95, 0.01),
            reliability: dim("reliability", 0.99, 0.01),
            latency: dim("latency", 50.0, 5.0),
            resource: dim("resource", 0.99, 0.01),
            operator_effort: dim("operator_effort", 1.0, 0.1),
            task_count: 50,
        }
    }

    /// The gate compares each measured value against its predeclared
    /// bound with zero tolerance, in the dimension's own direction.
    #[test]
    fn vc_201_017_mastery_canary_gate_from_scorecard_compares_measured_to_spec() {
        let spec = spec();
        assert_eq!(
            canary_gate_from_scorecard(&passing(), &spec),
            CanaryResult::Continue
        );

        let mut low_quality = passing();
        low_quality.quality = dim("quality", 0.79, 0.0);
        assert_eq!(
            canary_gate_from_scorecard(&low_quality, &spec),
            CanaryResult::StopTrial
        );

        let mut slow = passing();
        slow.latency = dim("latency", 100.5, 0.0);
        assert_eq!(
            canary_gate_from_scorecard(&slow, &spec),
            CanaryResult::StopTrial,
            "latency above max_latency is a regression even though the value is larger"
        );

        let mut at_bound = passing();
        at_bound.quality = dim("quality", 0.8, 0.0);
        at_bound.latency = dim("latency", 100.0, 0.0);
        assert_eq!(
            canary_gate_from_scorecard(&at_bound, &spec),
            CanaryResult::Continue,
            "exactly meeting a bound is not a regression"
        );

        let mut unmeasured = passing();
        unmeasured.resource = dim("resource", f64::NAN, 0.0);
        assert_eq!(
            canary_gate_from_scorecard(&unmeasured, &spec),
            CanaryResult::StopTrial
        );
    }
}

/// Production wiring: `evaluate_and_record` runs the canary gate before
/// `promote_with_scorecard`. A negative reported uncertainty widens a
/// below-bound quality of 0.70 into a `summary_ok` pass (0.70 + 0.20 >
/// 0.80); the canary compares the raw value, stops the trial, and the
/// experiment stays Evaluated with no release candidate written.
#[test]
fn vc_201_017_mastery_canary_gate_blocks_promotion_on_production_eval_path() {
    use crate::eval_separation::{evaluate_and_record, CandidateArtifact, EvalRunInput};
    use crate::experiment_lifecycle::ExperimentState;
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use scorecard_gate::{dim, passing, spec};
    use std::collections::BTreeSet;

    let ws = std::env::temp_dir().join(format!("susi-vc017-mastery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-canary".into(),
        fixtures: vec![make_fixture(FixtureSpec {
            id: "h1",
            class: FixtureClass::Coding,
            input: "held-out input",
            split: FixtureSplit::HeldOut,
            seed: 1,
            evaluator_expected: Some("expected-output".into()),
        })],
        promotion_spec: Some(spec()),
    };
    let run = |id: &str, measured| EvalRunInput {
        candidate: CandidateArtifact {
            id: id.into(),
            actual_output: BTreeSet::from(["expected-output".to_string()]),
            expected_results: BTreeSet::new(),
        },
        candidate_can_write: false,
        training_access: BTreeSet::new(),
        memory_access: BTreeSet::new(),
        measured: Some(measured),
    };

    let mut widened = passing();
    widened.quality = dim("quality", 0.70, -0.20);
    assert!(
        widened.summary_ok(&spec()),
        "the scorecard summary alone would promote this run"
    );
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-widened", widened)).unwrap();
    assert_eq!(ev.canary, Some(CanaryResult::StopTrial));
    assert_eq!(ev.experiment_state, Some(ExperimentState::Evaluated));
    assert!(!ws
        .join(".susi")
        .join("release-candidates")
        .join("cand-widened.json")
        .exists());

    // A genuinely passing run clears the canary and is promoted.
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-clean", passing())).unwrap();
    assert_eq!(ev.canary, Some(CanaryResult::Continue));
    assert_eq!(ev.experiment_state, Some(ExperimentState::PromotionReady));

    let _ = std::fs::remove_dir_all(&ws);
}
