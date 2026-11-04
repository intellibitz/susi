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

#[test]
fn vc_201_010_publish_empty_slice_returns_header_only() {
    let out = publish(&[]);
    assert_eq!(out, "rsi_comparisons:\n");
}

#[test]
fn vc_201_010_single_row_formats_candidate_baseline_delta_and_receipt() {
    let rows = vec![ExperimentComparison {
        candidate_id: "cand-alpha".into(),
        baseline_id: "base-main".into(),
        metric_delta: 0.125,
        receipt: "rcpt-99".into(),
        outcome: TrialOutcome::Success,
    }];
    let out = publish(&rows);
    assert!(out.contains("cand-alpha vs base-main"));
    assert!(out.contains("delta=0.1250"));
    assert!(out.contains("outcome=Success"));
    assert!(out.contains("receipt=rcpt-99"));
}

#[test]
fn vc_201_010_preserves_sign_and_four_decimal_precision_on_metric_delta() {
    let rows = vec![
        ExperimentComparison {
            candidate_id: "c-pos".into(),
            baseline_id: "b0".into(),
            metric_delta: 0.0005,
            receipt: "r-pos".into(),
            outcome: TrialOutcome::Success,
        },
        ExperimentComparison {
            candidate_id: "c-neg".into(),
            baseline_id: "b0".into(),
            metric_delta: -0.375,
            receipt: "r-neg".into(),
            outcome: TrialOutcome::Failed,
        },
    ];
    let out = publish(&rows);
    assert!(out.contains("delta=0.0005"), "out={out}");
    assert!(out.contains("delta=-0.3750"), "out={out}");
}
