use crate::eval_separation::{judge, CandidateArtifact, HeldOutSuite, PromotionGate};
use std::collections::BTreeSet;

#[test]
fn vc_201_005_write_permissions_unavailable_to_candidate() {
    let c = CandidateArtifact {
        id: "c1".into(),
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
        expected_results: BTreeSet::from(["real".into()]),
    };
    let s = HeldOutSuite {
        inputs: vec!["i1".into()],
        expected: BTreeSet::from(["real".into(), "extra".into()]),
    };
    assert_eq!(judge(&c, &s, false), PromotionGate::Pass);
}
