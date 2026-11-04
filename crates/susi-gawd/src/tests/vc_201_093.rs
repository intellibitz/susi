use crate::disaster_recovery::{restore_from_backup, BackupBundle, DrTargets};
use std::collections::BTreeMap;

#[test]
fn vc_201_093_restores_with_audit_continuity_and_key_policy() {
    let backup = BackupBundle {
        config: "cfg".into(),
        workflow_state: "wf".into(),
        keys: BTreeMap::from([("k1".into(), "secret".into())]),
        ledger_tail: "tail".into(),
        audit_anchor: "anchor-1".into(),
    };
    let no_keys = restore_from_backup(
        &backup,
        &DrTargets {
            rto_ms: 1000,
            rpo_records: 0,
            allow_key_restore: false,
        },
        500,
        0,
    );
    assert!(no_keys.audit_continuous);
    assert!(no_keys.within_rto && no_keys.within_rpo);
    assert!(no_keys.restored.keys.is_empty());

    let with_keys = restore_from_backup(
        &backup,
        &DrTargets {
            rto_ms: 100,
            rpo_records: 0,
            allow_key_restore: true,
        },
        500,
        2,
    );
    assert!(!with_keys.within_rto);
    assert!(!with_keys.within_rpo);
    assert_eq!(with_keys.restored.keys.len(), 1);
}

#[test]
fn vc_201_093_empty_ledger_tail_marks_audit_discontinuous() {
    let backup = BackupBundle {
        config: "cfg".into(),
        workflow_state: "wf".into(),
        keys: BTreeMap::new(),
        ledger_tail: String::new(),
        audit_anchor: "anchor-1".into(),
    };
    let report = restore_from_backup(
        &backup,
        &DrTargets {
            rto_ms: 500,
            rpo_records: 1,
            allow_key_restore: false,
        },
        100,
        0,
    );
    assert!(!report.audit_continuous);
}

#[test]
fn vc_201_093_exact_rto_and_rpo_boundaries_satisfy_targets() {
    let backup = BackupBundle {
        config: "cfg".into(),
        workflow_state: "wf".into(),
        keys: BTreeMap::new(),
        ledger_tail: "tail".into(),
        audit_anchor: "anchor-b".into(),
    };
    let report = restore_from_backup(
        &backup,
        &DrTargets {
            rto_ms: 250,
            rpo_records: 3,
            allow_key_restore: false,
        },
        250,
        3,
    );
    assert!(report.within_rto);
    assert!(report.within_rpo);
}

#[test]
fn vc_201_093_restored_bundle_preserves_config_workflow_and_ledger_tail() {
    let backup = BackupBundle {
        config: "config-v2".into(),
        workflow_state: "state-active".into(),
        keys: BTreeMap::from([("api".into(), "k-val".into())]),
        ledger_tail: "seq-99".into(),
        audit_anchor: "anchor-99".into(),
    };
    let report = restore_from_backup(
        &backup,
        &DrTargets {
            rto_ms: 1000,
            rpo_records: 5,
            allow_key_restore: true,
        },
        100,
        1,
    );
    assert_eq!(report.restored.config, "config-v2");
    assert_eq!(report.restored.workflow_state, "state-active");
    assert_eq!(report.restored.ledger_tail, "seq-99");
    assert_eq!(
        report.restored.keys.get("api").map(String::as_str),
        Some("k-val")
    );
}
