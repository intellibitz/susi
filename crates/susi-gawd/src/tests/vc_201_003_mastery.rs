//! Mastery verification for VC-201-003: a measurable improvement
//! scorecard gating on separate dimensions with predeclared thresholds
//! and reported uncertainty.
//!
//! The cited tests show dimensions improve individually. Verified
//! here: all five dimensions gate the verdict — resource and
//! operator_effort regressions block it just as quality, reliability
//! and latency misses do — an undersampled run cannot claim, bounds
//! live on the predeclared spec rather than the judged record, a
//! percent claim cannot rest on the task count, and the scorecard
//! gates the production promotion path.

use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};
use crate::scorecard::{
    DimensionScore, DimensionSpec, ImprovementBasis, ImprovementScorecard, ScorecardSpec,
};

fn dim(name: &str, value: f64, uncertainty: f64) -> DimensionScore {
    DimensionScore {
        name: name.into(),
        value,
        uncertainty,
    }
}

/// Bounds fixed before the run: quality, reliability and resource
/// efficiency improve upward; latency and operator effort downward.
fn spec() -> ScorecardSpec {
    ScorecardSpec {
        quality: DimensionSpec {
            threshold: 0.8,
            lower_is_better: false,
        },
        reliability: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        latency: DimensionSpec {
            threshold: 100.0,
            lower_is_better: true,
        },
        resource: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        operator_effort: DimensionSpec {
            threshold: 5.0,
            lower_is_better: true,
        },
        min_task_count: 30,
    }
}

/// Every dimension clears its bound with margin, on enough tasks.
fn passing() -> ImprovementScorecard {
    ImprovementScorecard {
        quality: dim("quality", 0.95, 0.01),
        reliability: dim("reliability", 0.99, 0.01),
        latency: dim("latency", 50.0, 5.0),
        resource: dim("resource", 0.99, 0.01),
        operator_effort: dim("operator_effort", 1.0, 0.1),
        task_count: 50,
    }
}

/// A regression on any of the five dimensions blocks the verdict —
/// including the two that were previously invisible to it.
#[test]
fn vc_201_003_mastery_all_five_dimensions_gate_the_verdict() {
    let spec = spec();
    assert!(passing().summary_ok(&spec), "clean run improves");

    for (slot, regressed) in [
        ("quality", dim("quality", 0.1, 0.0)),
        ("reliability", dim("reliability", 0.1, 0.0)),
        ("latency", dim("latency", 500.0, 0.0)),
        ("resource", dim("resource", 0.0, 0.0)),
        ("operator_effort", dim("operator_effort", 99.0, 0.0)),
    ] {
        let mut s = passing();
        match slot {
            "quality" => s.quality = regressed,
            "reliability" => s.reliability = regressed,
            "latency" => s.latency = regressed,
            "resource" => s.resource = regressed,
            "operator_effort" => s.operator_effort = regressed,
            _ => unreachable!("fixed slot list"),
        }
        assert!(
            !s.summary_ok(&spec),
            "a {slot} regression must block the verdict"
        );
    }
}

/// Undersampled cannot claim: a run below the predeclared floor fails
/// even when every measurement passes, and a floor set to zero still
/// demands at least one observed task.
#[test]
fn vc_201_003_mastery_undersampled_cannot_claim() {
    let spec = spec();

    let mut s = passing();
    s.task_count = 0;
    assert!(
        !s.summary_ok(&spec),
        "a scorecard built on zero tasks cannot declare improvement"
    );

    s.task_count = spec.min_task_count - 1;
    assert!(!s.summary_ok(&spec), "below the floor is undersampled");
    assert!(!s.sufficiently_sampled(&spec));

    s.task_count = spec.min_task_count;
    assert!(s.summary_ok(&spec), "at the floor the run can claim");

    // A spec may not set the floor below one task.
    let lax = ScorecardSpec {
        min_task_count: 0,
        ..spec
    };
    s.task_count = 0;
    assert!(!s.summary_ok(&lax), "zero tasks is always undersampled");
}

/// Thresholds are predeclared on the spec, not carried by the judged
/// record: the same measurement is judged against the bound that was
/// fixed beforehand, and the record itself holds no goalpost an
/// after-the-fact edit could move.
#[test]
fn vc_201_003_mastery_thresholds_live_on_the_predeclared_spec() {
    let mut s = passing();
    s.quality = dim("quality", 0.82, 0.01);
    s.task_count = spec().min_task_count;

    // Declared bound 0.9: the measurement misses, and no field on the
    // scorecard can be edited to lower it.
    let declared = ScorecardSpec {
        quality: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        ..spec()
    };
    assert!(
        !s.summary_ok(&declared),
        "quality missed its declared bound"
    );

    // A *different* predeclaration judges the same record differently —
    // the verdict follows the spec, never the record.
    let lenient = spec();
    assert!(s.summary_ok(&lenient));

    let serialized = serde_json::to_value(&s).unwrap();
    for slot in [
        "quality",
        "reliability",
        "latency",
        "resource",
        "operator_effort",
    ] {
        assert!(
            serialized[slot].get("threshold").is_none(),
            "the judged record carries no {slot} goalpost"
        );
    }
}

/// A percent improvement may rest only on a measured dimension; a
/// claim whose basis is how many tasks ran is refused outright.
#[test]
fn vc_201_003_mastery_task_count_cannot_pass_as_a_percent() {
    let s = passing();
    assert!(s.refuses_task_count_as_percent(ImprovementBasis::TaskCount));
    assert!(!s.refuses_task_count_as_percent(ImprovementBasis::MeasuredDimension));
}

/// The scorecard is consumed on the production promotion path: an
/// experiment reaches PromotionReady through `promote_with_scorecard`
/// only when the verdict clears the predeclared spec; a failing or
/// undersampled verdict leaves the experiment Evaluated.
#[test]
fn vc_201_003_mastery_scorecard_gates_the_promotion_path() {
    let spec = spec();
    let mut log = ExperimentLog::default();
    log.propose("exp-1").unwrap();
    log.transition("exp-1", ExperimentState::Isolated).unwrap();
    log.transition("exp-1", ExperimentState::Evaluated).unwrap();

    // Resource regressed: promotion refused, state untouched.
    let mut regressed = passing();
    regressed.resource = dim("resource", 0.0, 0.0);
    assert!(log
        .promote_with_scorecard("exp-1", &regressed, &spec)
        .is_err());
    assert_eq!(
        log.experiments["exp-1"].state,
        ExperimentState::Evaluated,
        "a failed verdict cannot promote"
    );

    // Undersampled: also refused.
    let mut thin = passing();
    thin.task_count = 0;
    assert!(log.promote_with_scorecard("exp-1", &thin, &spec).is_err());
    assert_eq!(log.experiments["exp-1"].state, ExperimentState::Evaluated);

    // Clean verdict: the transition applies.
    log.promote_with_scorecard("exp-1", &passing(), &spec)
        .unwrap();
    assert_eq!(
        log.experiments["exp-1"].state,
        ExperimentState::PromotionReady
    );
}

/// What still holds: an uncertainty band covering the bound blocks a
/// marginal claim; tightening the band over the same value lets it
/// through.
#[test]
fn vc_201_003_mastery_uncertainty_blocks_a_marginal_claim() {
    let spec = spec();
    let mut s = passing();
    s.quality = dim("quality", 0.81, 0.05); // band covers the 0.8 bound
    assert!(!s.summary_ok(&spec), "wide uncertainty blocks the claim");

    s.quality = dim("quality", 0.81, 0.005);
    assert!(s.summary_ok(&spec), "a tight band clears the same value");
}

/// The scorecard gates the *production* promotion path — `evaluate_and_record`
/// (the seam `susi tasks eval-corpus` runs): the corpus declares the spec
/// before any run, the run supplies measured dimensions, and only a run
/// whose gate passes *and* whose measurements clear the declared bounds
/// lands `PromotionReady`.
#[test]
fn vc_201_003_mastery_production_eval_path_gates_promotion() {
    use crate::eval_separation::{
        evaluate_and_record, CandidateArtifact, EvalRunInput, EvalVerdict,
    };
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use std::collections::BTreeSet;

    let root = std::env::temp_dir().join(format!("susi-vc003-prod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-1".into(),
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

    let run = |id: &str, measured: Option<ImprovementScorecard>| EvalRunInput {
        candidate: CandidateArtifact {
            id: id.into(),
            actual_output: BTreeSet::from(["expected-output".to_string()]),
            expected_results: BTreeSet::new(),
        },
        candidate_can_write: false,
        training_access: BTreeSet::new(),
        memory_access: BTreeSet::new(),
        measured,
    };

    // Gate pass + a scorecard clearing the declared spec -> PromotionReady,
    // and durably so: a fresh log load sees the same state.
    let ev = evaluate_and_record(&root, &corpus, &run("cand-a", Some(passing()))).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::Pass);
    assert_eq!(ev.experiment_state, Some(ExperimentState::PromotionReady));
    let log = ExperimentLog::load(root.join(".susi").join("experiments"));
    assert_eq!(
        log.experiments["cand-a"].state,
        ExperimentState::PromotionReady
    );

    // Gate pass but a measured scorecard missing one declared bound ->
    // stays Evaluated, never promoted.
    let mut regressed = passing();
    regressed.resource = dim("resource", 0.0, 0.0);
    let ev = evaluate_and_record(&root, &corpus, &run("cand-b", Some(regressed))).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::Pass);
    assert_eq!(ev.experiment_state, Some(ExperimentState::Evaluated));

    // Gate pass carrying no measurements -> Evaluated: a bare verdict is
    // no improvement claim.
    let ev = evaluate_and_record(&root, &corpus, &run("cand-c", None)).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Evaluated));

    // A failed verdict rejects the experiment outright.
    let mut failed = run("cand-d", Some(passing()));
    failed.candidate.actual_output = BTreeSet::new();
    let ev = evaluate_and_record(&root, &corpus, &failed).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::FailWrongOutput);
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));

    // A terminal state stands: re-evaluating the rejected id does not
    // reopen it — a revised candidate earns a new id.
    let ev = evaluate_and_record(&root, &corpus, &run("cand-d", Some(passing()))).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));

    let _ = std::fs::remove_dir_all(&root);
}
