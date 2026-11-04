use crate::eval_separation::{judge, CandidateArtifact, HeldOutSuite, PromotionGate};
use std::collections::BTreeSet;

#[test]
fn vc_201_005_write_permissions_unavailable_to_candidate() {
    let c = CandidateArtifact {
        id: "c1".into(),
        actual_output: BTreeSet::from(["ok".into()]),
        expected_results: BTreeSet::from(["ok".into()]),
    };
    let s = HeldOutSuite {
        inputs: vec!["i1".into()],
        expected: BTreeSet::from(["ok".into()]),
    };
    assert_eq!(judge(&c, &s, true), PromotionGate::FailWriteAttempt);
}

#[test]
fn vc_201_005_self_altered_expectations_fail_gate() {
    let c = CandidateArtifact {
        id: "c1".into(),
        actual_output: BTreeSet::from(["real".into()]),
        expected_results: BTreeSet::from(["forged-pass".into()]),
    };
    let s = HeldOutSuite {
        inputs: vec!["i1".into()],
        expected: BTreeSet::from(["real".into()]),
    };
    assert_eq!(
        judge(&c, &s, false),
        PromotionGate::FailSelfAlteredExpectations
    );
}

#[test]
fn vc_201_005_honest_candidate_passes() {
    let c = CandidateArtifact {
        id: "c1".into(),
        actual_output: BTreeSet::from(["real".into(), "extra".into()]),
        expected_results: BTreeSet::from(["real".into()]),
    };
    let s = HeldOutSuite {
        inputs: vec!["i1".into()],
        expected: BTreeSet::from(["real".into(), "extra".into()]),
    };
    assert_eq!(judge(&c, &s, false), PromotionGate::Pass);
}

#[test]
fn vc_201_005_missing_expected_output_fails_wrong_output() {
    let c = CandidateArtifact {
        id: "c-incomplete".into(),
        actual_output: BTreeSet::from(["only-one".into()]),
        expected_results: BTreeSet::from(["only-one".into()]),
    };
    let s = HeldOutSuite {
        inputs: vec!["i1".into(), "i2".into()],
        expected: BTreeSet::from(["only-one".into(), "missing-two".into()]),
    };
    assert_eq!(judge(&c, &s, false), PromotionGate::FailWrongOutput);
    assert_eq!(judge(&c, &s, true), PromotionGate::FailWriteAttempt);
}

#[test]
fn vc_201_005_empty_held_out_suite_passes_for_unmodified_candidate() {
    let c = CandidateArtifact {
        id: "c-empty".into(),
        actual_output: BTreeSet::new(),
        expected_results: BTreeSet::new(),
    };
    let s = HeldOutSuite {
        inputs: Vec::new(),
        expected: BTreeSet::new(),
    };
    assert_eq!(judge(&c, &s, false), PromotionGate::Pass);
}
