use crate::experiment_memory::{ExperimentMemory, ExperimentMemoryEntry};

#[test]
fn vc_201_019_blocks_repeat_without_new_evidence() {
    let mut m = ExperimentMemory::default();
    m.record(ExperimentMemoryEntry {
        proposal_id: "p1".into(),
        cause: "gate fail".into(),
        counterexample: Some("case-9".into()),
        lineage: vec!["root".into()],
    });
    assert!(!m.may_resubmit("p1", false, false));
    assert!(m.may_resubmit("p1", true, false));
    assert!(m.may_resubmit("p1", false, true));
    assert!(m.may_resubmit("fresh", false, false));
}
