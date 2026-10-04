use crate::experiment_memory::{ExperimentMemory, ExperimentMemoryEntry};

fn entry(proposal_id: &str, cause: &str, counterexample: Option<&str>) -> ExperimentMemoryEntry {
    ExperimentMemoryEntry {
        proposal_id: proposal_id.into(),
        cause: cause.into(),
        counterexample: counterexample.map(String::from),
        lineage: vec!["root".into()],
    }
}

#[test]
fn vc_201_019_blocks_repeat_without_new_evidence() {
    let mut m = ExperimentMemory::default();
    m.record(ExperimentMemoryEntry {
        proposal_id: "p1".into(),
        cause: "gate fail".into(),
        counterexample: Some("case-9".into()),
        lineage: vec!["root".into()],
    });
    assert!(!m.may_resubmit(&entry("p1", "gate fail", Some("case-9")), &[], &[]));
    assert!(m.may_resubmit(
        &entry("p1", "gate fail", Some("case-9")),
        &["p1".into()],
        &[]
    ));
    assert!(m.may_resubmit(
        &entry("p1", "gate fail", Some("case-9")),
        &[],
        &["new failure mode found".into()]
    ));
    assert!(m.may_resubmit(&entry("fresh", "other cause", None), &[], &[]));
}
