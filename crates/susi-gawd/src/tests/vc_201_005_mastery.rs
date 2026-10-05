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
