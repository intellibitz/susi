use crate::zc_debt_report::{format_debt_report, scan_debt, DebtItem, DebtKind};

#[test]
fn zc_debt_report_lists_manual_inputs_with_removal_tasks() {
    let items = scan_debt(&[DebtItem {
        id: "env:SUSI_EXTRA".into(),
        kind: DebtKind::EnvVar,
        description: "legacy user-facing env".into(),
        removes_via: "T-CLAUDE-156".into(),
    }]);
    assert!(items.iter().any(|i| i.id == "secret:cloud_api_key"));
    assert!(items.iter().any(|i| i.id == "env:SUSI_EXTRA"));
    let report = format_debt_report(&items);
    assert!(report.contains("zero-config debt:"));
    assert!(report.contains("removes via T-CLAUDE-33"));
    assert!(report.contains("removes via T-CLAUDE-156"));
}

#[test]
fn zc_debt_report_empty_when_no_debt() {
    assert_eq!(format_debt_report(&[]), "zero-config debt: none\n");
}
