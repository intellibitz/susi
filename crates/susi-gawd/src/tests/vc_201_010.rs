use crate::rsi_comparisons::{publish, ExperimentComparison, TrialOutcome};

#[test]
fn vc_201_010_publishes_all_outcomes() {
    let rows = vec![
        ExperimentComparison {
            candidate_id: "c1".into(),
            baseline_id: "b0".into(),
            metric_delta: 0.1,
            receipt: "r1".into(),
            outcome: TrialOutcome::Success,
        },
        ExperimentComparison {
            candidate_id: "c2".into(),
            baseline_id: "b0".into(),
            metric_delta: -0.2,
            receipt: "r2".into(),
            outcome: TrialOutcome::Failed,
        },
        ExperimentComparison {
            candidate_id: "c3".into(),
            baseline_id: "b0".into(),
            metric_delta: 0.0,
            receipt: "r3".into(),
            outcome: TrialOutcome::Inconclusive,
        },
        ExperimentComparison {
            candidate_id: "c4".into(),
            baseline_id: "b0".into(),
            metric_delta: 0.05,
            receipt: "r4".into(),
            outcome: TrialOutcome::Rejected,
        },
    ];
    let s = publish(&rows);
    assert!(s.contains("Success"));
    assert!(s.contains("Failed"));
    assert!(s.contains("Inconclusive"));
    assert!(s.contains("Rejected"));
    assert!(s.contains("receipt=r2"));
}
