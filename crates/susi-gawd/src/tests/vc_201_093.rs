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
