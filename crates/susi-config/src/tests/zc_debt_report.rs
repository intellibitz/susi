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

#[test]
fn zc_debt_report_deduplicates_duplicate_extra_ids_and_sorts_by_id() {
    let items = scan_debt(&[
        DebtItem {
            id: "env:SUSI_DUP".into(),
            kind: DebtKind::EnvVar,
            description: "first".into(),
            removes_via: "T-1".into(),
        },
        DebtItem {
            id: "env:SUSI_DUP".into(),
            kind: DebtKind::EnvVar,
            description: "second".into(),
            removes_via: "T-2".into(),
        },
    ]);
    assert_eq!(items.iter().filter(|i| i.id == "env:SUSI_DUP").count(), 1);
    assert!(items.windows(2).all(|w| w[0].id <= w[1].id));
}

#[test]
fn zc_debt_report_renders_all_five_debt_kinds_with_lowercase_badges() {
    let items = [
        DebtItem {
            id: "a".into(),
            kind: DebtKind::Secret,
            description: "secret desc".into(),
            removes_via: "T-A".into(),
        },
        DebtItem {
            id: "b".into(),
            kind: DebtKind::Consent,
            description: "consent desc".into(),
            removes_via: "T-B".into(),
        },
        DebtItem {
            id: "c".into(),
            kind: DebtKind::EnvVar,
            description: "envvar desc".into(),
            removes_via: "T-C".into(),
        },
        DebtItem {
            id: "d".into(),
            kind: DebtKind::Prompt,
            description: "prompt desc".into(),
            removes_via: "T-D".into(),
        },
        DebtItem {
            id: "e".into(),
            kind: DebtKind::SetupStep,
            description: "setupstep desc".into(),
            removes_via: "T-E".into(),
        },
    ];
    let rendered = format_debt_report(&items);
    for badge in [
        "[secret]",
        "[consent]",
        "[envvar]",
        "[prompt]",
        "[setupstep]",
    ] {
        assert!(
            rendered.contains(badge),
            "missing badge {badge} in {rendered}"
        );
    }
}

#[test]
fn zc_debt_report_scan_with_no_extras_is_sorted_and_non_empty() {
    let base = scan_debt(&[]);
    assert!(!base.is_empty());
    assert!(base.windows(2).all(|w| w[0].id <= w[1].id));
    for item in &base {
        assert!(!item.id.is_empty());
        assert!(!item.description.is_empty());
        assert!(!item.removes_via.is_empty());
    }
}
