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

#[test]
fn vc_201_019_empty_memory_allows_any_proposal() {
    let m = ExperimentMemory::default();
    assert!(m.may_resubmit(&entry("p-any", "any cause", Some("cx-1")), &[], &[]));
    assert!(m.may_resubmit(&entry("p-none", "other", None), &[], &[]));
}

#[test]
fn vc_201_019_blocks_multiple_distinct_proposals_and_renamed_duplicates() {
    let mut m = ExperimentMemory::default();
    m.record(entry("p-alpha", "oom in benchmark", Some("bench-4")));
    m.record(entry("p-beta", "deadlock on shutdown", None));

    assert!(!m.may_resubmit(
        &entry("p-alpha", "oom in benchmark", Some("bench-4")),
        &[],
        &[]
    ));
    assert!(!m.may_resubmit(&entry("p-beta", "deadlock on shutdown", None), &[], &[]));
    // Renamed proposal with identical (cause, counterexample) is also blocked unless prior id is addressed
    assert!(!m.may_resubmit(
        &entry("p-alpha-renamed", "oom in benchmark", Some("bench-4")),
        &[],
        &[]
    ));
    assert!(m.may_resubmit(
        &entry("p-alpha-renamed", "oom in benchmark", Some("bench-4")),
        &["p-alpha".into()],
        &[]
    ));
}

#[test]
fn vc_201_019_preserves_lineage_and_rejects_blank_fresh_receipts() {
    let mut m = ExperimentMemory::default();
    let recorded = ExperimentMemoryEntry {
        proposal_id: "p-lineage".into(),
        cause: "regression".into(),
        counterexample: Some("test-7".into()),
        lineage: vec!["anc-0".into(), "anc-1".into()],
    };
    m.record(recorded.clone());
    assert_eq!(
        m.by_proposal["p-lineage"].lineage,
        vec!["anc-0".to_string(), "anc-1".to_string()]
    );
    assert!(!m.may_resubmit(&recorded, &[], &["   ".into()]));
}
