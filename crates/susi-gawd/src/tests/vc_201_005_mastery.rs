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

/// Falsification: the gate judges declared expectations, not output.
/// CandidateArtifact has no field for what the candidate actually
/// produced — so a patch whose output is *wrong*, as long as it declares
/// expected_results that are a subset of the suite's, passes. The
/// 'evaluation' compares claims, not results.
#[test]
fn vc_201_005_mastery_wrong_output_still_passes_the_gate() {
    // The candidate declares a subset of the suite's expectations. Its
    // actual output — whatever it was — is never examined; there is no
    // field for it.
    let c = CandidateArtifact {
        id: "c-empty-run".into(),
        expected_results: BTreeSet::from(["real".into()]),
    };
    assert_eq!(
        judge(&c, &suite(&["real", "other"]), false),
        PromotionGate::Pass,
        "a candidate passes on declared expectations with no output check"
    );
}

/// Falsification: a candidate that produced nothing passes. An empty
/// expected_results set is trivially a subset — the gate cannot tell
/// 'did nothing' from 'satisfied the suite'.
#[test]
fn vc_201_005_mastery_empty_candidate_passes() {
    let c = CandidateArtifact {
        id: "did-nothing".into(),
        expected_results: BTreeSet::new(),
    };
    assert_eq!(
        judge(&c, &suite(&["real"]), false),
        PromotionGate::Pass,
        "a candidate with no results satisfies the promotion gate"
    );
}

/// Falsification: write-permission separation is asserted, not enforced.
/// `candidate_can_write` is a bool the *caller* supplies; the candidate
/// artifact is a plain struct — nothing prevents the same code path from
/// running the gate with the flag simply flipped.
#[test]
fn vc_201_005_mastery_write_boundary_is_a_caller_flag() {
    let c = CandidateArtifact {
        id: "c1".into(),
        expected_results: BTreeSet::from(["real".into()]),
    };
    // Same artifact, same suite — the 'separation' flips with a bool the
    // harness passes, not with any isolation the gate can observe.
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
        expected_results: BTreeSet::from(["forged-pass".into()]),
    };
    assert_eq!(
        judge(&forged, &suite(&["real"]), false),
        PromotionGate::FailSelfAlteredExpectations
    );
}
