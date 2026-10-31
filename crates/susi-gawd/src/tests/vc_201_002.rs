use crate::baseline_capture::BaselineRecord;

#[test]
fn vc_201_002_release_baseline_unchanged_reference() {
    let b = BaselineRecord::release_reference("abc123", "cpus=8", "susi brain bench")
        .with_metrics(0.95, 120, 400, 0);
    assert!(b.is_release_reference);
    assert_eq!(b.runtime, "release");
    assert_eq!(b.commit, "abc123");
    assert!(!b.command.is_empty());
}

#[test]
fn vc_201_002_candidate_runs_on_dev_instance() {
    let c = BaselineRecord::candidate_on_dev("def456", "cpus=8", "susi brain bench")
        .with_metrics(0.96, 110, 410, 0);
    assert!(!c.is_release_reference);
    assert_eq!(c.runtime, "dev-instance");
}

#[test]
fn vc_201_002_records_required_fields_for_comparison() {
    let a = BaselineRecord::release_reference("a", "hw", "cmd").with_metrics(1.0, 1, 1, 1);
    let b = BaselineRecord::candidate_on_dev("b", "hw", "cmd").with_metrics(1.0, 2, 2, 2);
    assert!(a.is_comparable_to(&b));
    assert!(a.tokens > 0 || a.latency_ms > 0 || a.success_rate >= 0.0);
}
