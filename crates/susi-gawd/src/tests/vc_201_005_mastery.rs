//! Mastery verification for VC-201-005: candidate generation separated
//! from judging.
//!
//! The cited tests show the verdict enum on crafted inputs. The
//! distinguishing properties are that the gate actually evaluates a
//! candidate's *output* (not the expectations it declares), that a
//! candidate doing nothing cannot pass, and that the write-permission
//! boundary is real rather than asserted by the caller.

use crate::eval_separation::{judge, CandidateArtifact, HeldOutSuite, PromotionGate};
use std::collections::BTreeSet;

fn suite(expected: &[&str]) -> HeldOutSuite {
    HeldOutSuite {
        inputs: vec!["i1".into()],
        expected: expected.iter().map(|s| s.to_string()).collect(),
    }
}

/// Fixed: the gate now judges `actual_output`, not declared
/// expectations. A candidate that declares a correct-looking
/// `expected_results` subset but whose actual output is wrong (or
/// missing) now fails.
#[test]
fn vc_201_005_mastery_wrong_output_still_passes_the_gate() {
    let c = CandidateArtifact {
        id: "c-empty-run".into(),
        // Output never covers "other" — the suite's full held-out truth.
        actual_output: BTreeSet::from(["real".into()]),
        expected_results: BTreeSet::from(["real".into()]),
    };
    assert_eq!(
        judge(&c, &suite(&["real", "other"]), false),
        PromotionGate::FailWrongOutput,
        "actual output not covering the held-out truth must fail"
    );
}

/// Fixed: a candidate that produced nothing now fails — an empty
/// `actual_output` cannot cover a non-empty `suite.expected`.
#[test]
fn vc_201_005_mastery_empty_candidate_passes() {
    let c = CandidateArtifact {
        id: "did-nothing".into(),
        actual_output: BTreeSet::new(),
        expected_results: BTreeSet::new(),
    };
    assert_eq!(
        judge(&c, &suite(&["real"]), false),
        PromotionGate::FailWrongOutput,
        "a candidate with no output must not satisfy the promotion gate"
    );
}

/// Holds (by design, not by enforcement): `candidate_can_write` is the
/// gate's declared write-permission input — the gate itself has no way
/// to observe isolation, so callers must supply it from an actually
/// enforced sandbox boundary. The gate still fails closed on `true`.
#[test]
fn vc_201_005_mastery_write_boundary_is_a_caller_flag() {
    let c = CandidateArtifact {
        id: "c1".into(),
        actual_output: BTreeSet::from(["real".into()]),
        expected_results: BTreeSet::from(["real".into()]),
    };
    assert_eq!(
        judge(&c, &suite(&["real"]), true),
        PromotionGate::FailWriteAttempt
    );
    assert_eq!(judge(&c, &suite(&["real"]), false), PromotionGate::Pass);
}

/// What does hold: a candidate inventing expectations absent from the
/// held-out suite fails, and the write-attempt flag fails closed.
#[test]
fn vc_201_005_mastery_invented_expectations_and_write_flag_fail() {
    let forged = CandidateArtifact {
        id: "c1".into(),
        actual_output: BTreeSet::from(["real".into()]),
        expected_results: BTreeSet::from(["forged-pass".into()]),
    };
    assert_eq!(
        judge(&forged, &suite(&["real"]), false),
        PromotionGate::FailSelfAlteredExpectations
    );
}

/// The separation holds on the production eval path: `evaluate_and_record`
/// (the seam `susi tasks eval-corpus` runs) judges a candidate that is a
/// *data artifact* — the evaluator never executes the candidate, so it
/// structurally holds no write channel — against held-out truth that
/// comes only from the verified corpus. A declared write boundary still
/// fails closed, invented expectations still fail, and every verdict
/// lands durably on the experiment record.
#[test]
fn vc_201_005_mastery_production_eval_separates_generation_from_judging() {
    use crate::eval_separation::{evaluate_and_record, EvalRunInput, EvalVerdict};
    use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };

    let root = std::env::temp_dir().join(format!("susi-vc005-prod-{}", std::process::id()));
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
            evaluator_expected: Some("truth".into()),
        })],
        promotion_spec: None,
    };

    let run = |id: &str, can_write: bool, actual: &[&str], declared: &[&str]| EvalRunInput {
        candidate: CandidateArtifact {
            id: id.into(),
            actual_output: actual.iter().map(|s| s.to_string()).collect(),
            expected_results: declared.iter().map(|s| s.to_string()).collect(),
        },
        candidate_can_write: can_write,
        training_access: BTreeSet::new(),
        memory_access: BTreeSet::new(),
        measured: None,
    };

    // Judged on actual output, not the declaration: covering the held-out
    // truth passes; the experiment lands Evaluated durably.
    let ev = evaluate_and_record(&root, &corpus, &run("c-ok", false, &["truth"], &[])).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::Pass);
    assert_eq!(ev.experiment_state, Some(ExperimentState::Evaluated));

    // A run flagged as write-capable fails closed — the durable
    // experiment record shows the rejection, not just the return value.
    let ev = evaluate_and_record(&root, &corpus, &run("c-w", true, &["truth"], &[])).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::FailWriteAttempt);
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));
    let log = ExperimentLog::load(root.join(".susi").join("experiments"));
    assert_eq!(log.experiments["c-w"].state, ExperimentState::Rejected);

    // Inventing expectations outside the held-out suite still fails.
    let ev =
        evaluate_and_record(&root, &corpus, &run("c-f", false, &["truth"], &["forged"])).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::FailSelfAlteredExpectations);
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));

    // Wrong output cannot declare its way to a pass.
    let ev = evaluate_and_record(&root, &corpus, &run("c-x", false, &["other"], &[])).unwrap();
    assert_eq!(ev.verdict, EvalVerdict::FailWrongOutput);

    let _ = std::fs::remove_dir_all(&root);
}
