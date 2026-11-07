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

#[test]
fn vc_201_093_distinguishes_independent_rto_and_rpo_breaches_and_records_metrics() {
    let backup = BackupBundle {
        config: "cfg".into(),
        workflow_state: "wf".into(),
        keys: BTreeMap::new(),
        ledger_tail: "tail-present".into(),
        audit_anchor: "anchor-ok".into(),
    };
    let targets = DrTargets {
        rto_ms: 500,
        rpo_records: 2,
        allow_key_restore: false,
    };
    let rto_breach = restore_from_backup(&backup, &targets, 501, 2);
    assert!(!rto_breach.within_rto);
    assert!(rto_breach.within_rpo);
    assert_eq!(rto_breach.recovery_ms, 501);
    assert_eq!(rto_breach.data_loss_records, 2);

    let rpo_breach = restore_from_backup(&backup, &targets, 500, 3);
    assert!(rpo_breach.within_rto);
    assert!(!rpo_breach.within_rpo);
    assert_eq!(rpo_breach.recovery_ms, 500);
    assert_eq!(rpo_breach.data_loss_records, 3);
}

#[test]
fn isolated_node_restore_worker_entrypoint() {
    let Ok(node_dir) = std::env::var("SUSI_DR_ISOLATED_NODE_DIR") else {
        return;
    };
    let node_path = std::path::PathBuf::from(node_dir);
    let backup_raw = std::fs::read_to_string(node_path.join("backup_bundle.json")).unwrap();
    let targets_raw = std::fs::read_to_string(node_path.join("dr_targets.json")).unwrap();
    let source_ledger_raw = std::fs::read_to_string(node_path.join("source_ledger.jsonl")).unwrap();

    let backup: BackupBundle = serde_json::from_str(&backup_raw).unwrap();
    let targets: DrTargets = serde_json::from_str(&targets_raw).unwrap();

    let started = std::time::Instant::now();
    let probe = restore_from_backup(&backup, &targets, 0, 0);

    // Materialize restored state on the isolated node filesystem.
    std::fs::write(node_path.join("config.toml"), &probe.restored.config).unwrap();
    std::fs::write(
        node_path.join("workflow_state.json"),
        &probe.restored.workflow_state,
    )
    .unwrap();
    std::fs::write(
        node_path.join("keys.json"),
        serde_json::to_string(&probe.restored.keys).unwrap(),
    )
    .unwrap();
    std::fs::write(node_path.join("ledger.jsonl"), &probe.restored.ledger_tail).unwrap();
    std::fs::write(
        node_path.join("audit_anchor.txt"),
        &probe.restored.audit_anchor,
    )
    .unwrap();

    let source_records = source_ledger_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count() as u64;
    let restored_records = probe
        .restored
        .ledger_tail
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count() as u64;
    let lost_records = source_records.saturating_sub(restored_records);
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let final_report = restore_from_backup(&backup, &targets, elapsed_ms, lost_records);
    std::fs::write(
        node_path.join("restore_report.json"),
        serde_json::to_string_pretty(&final_report).unwrap(),
    )
    .unwrap();
}

#[test]
fn restores_to_isolated_node_and_reports_rto_rpo_against_targets() {
    let base_dir =
        std::env::temp_dir().join(format!("susi_vc_201_093_isolated_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base_dir);
    let node_no_keys = base_dir.join("node_no_keys");
    let node_with_keys = base_dir.join("node_with_keys");
    std::fs::create_dir_all(&node_no_keys).unwrap();
    std::fs::create_dir_all(&node_with_keys).unwrap();

    let ledger_history = "{\"seq\":1,\"event\":\"genesis\"}\n{\"seq\":2,\"event\":\"checkpoint\"}\n{\"seq\":3,\"event\":\"commit\"}\n";
    let backup = BackupBundle {
        config: "node_mode = \"isolated_dr\"\nquorum = 3\n".into(),
        workflow_state: "{\"active_mission\":\"m-dr-93\",\"step\":4}".into(),
        keys: BTreeMap::from([
            (
                "cluster_signing".into(),
                "ed25519:secret-key-material".into(),
            ),
            ("vault_seal".into(), "seal:key-2".into()),
        ]),
        ledger_tail: ledger_history.into(),
        audit_anchor: "sha256:93f00ba1c0ffee".into(),
    };

    // 1. Restore to isolated node A (separate OS process) with key restoration disabled by policy.
    let targets_no_keys = DrTargets {
        rto_ms: 15_000,
        rpo_records: 0,
        allow_key_restore: false,
    };
    std::fs::write(
        node_no_keys.join("backup_bundle.json"),
        serde_json::to_string(&backup).unwrap(),
    )
    .unwrap();
    std::fs::write(
        node_no_keys.join("dr_targets.json"),
        serde_json::to_string(&targets_no_keys).unwrap(),
    )
    .unwrap();
    std::fs::write(node_no_keys.join("source_ledger.jsonl"), ledger_history).unwrap();

    let outer_start = std::time::Instant::now();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("vc_201_093_tests::isolated_node_restore_worker_entrypoint")
        .env("SUSI_DR_ISOLATED_NODE_DIR", &node_no_keys)
        .status()
        .unwrap();
    let outer_elapsed_ms = outer_start.elapsed().as_millis() as u64;
    assert!(status.success(), "isolated DR worker process must succeed");

    let report_no_keys: crate::disaster_recovery::RestoreReport = serde_json::from_str(
        &std::fs::read_to_string(node_no_keys.join("restore_report.json")).unwrap(),
    )
    .unwrap();
    assert!(report_no_keys.audit_continuous);
    assert!(report_no_keys.within_rto);
    assert!(report_no_keys.within_rpo);
    assert!(report_no_keys.recovery_ms <= targets_no_keys.rto_ms);
    assert!(outer_elapsed_ms <= targets_no_keys.rto_ms);
    assert_eq!(report_no_keys.data_loss_records, 0);
    assert!(report_no_keys.restored.keys.is_empty());
    assert_eq!(
        std::fs::read_to_string(node_no_keys.join("config.toml")).unwrap(),
        backup.config
    );
    assert_eq!(
        std::fs::read_to_string(node_no_keys.join("workflow_state.json")).unwrap(),
        backup.workflow_state
    );
    assert_eq!(
        std::fs::read_to_string(node_no_keys.join("ledger.jsonl")).unwrap(),
        backup.ledger_tail
    );
    assert_eq!(
        std::fs::read_to_string(node_no_keys.join("audit_anchor.txt")).unwrap(),
        backup.audit_anchor
    );

    // 2. Restore to isolated node B (separate OS process) with key restoration allowed under policy.
    let targets_with_keys = DrTargets {
        rto_ms: 15_000,
        rpo_records: 0,
        allow_key_restore: true,
    };
    std::fs::write(
        node_with_keys.join("backup_bundle.json"),
        serde_json::to_string(&backup).unwrap(),
    )
    .unwrap();
    std::fs::write(
        node_with_keys.join("dr_targets.json"),
        serde_json::to_string(&targets_with_keys).unwrap(),
    )
    .unwrap();
    std::fs::write(node_with_keys.join("source_ledger.jsonl"), ledger_history).unwrap();

    let status_with_keys = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("vc_201_093_tests::isolated_node_restore_worker_entrypoint")
        .env("SUSI_DR_ISOLATED_NODE_DIR", &node_with_keys)
        .status()
        .unwrap();
    assert!(status_with_keys.success());

    let report_with_keys: crate::disaster_recovery::RestoreReport = serde_json::from_str(
        &std::fs::read_to_string(node_with_keys.join("restore_report.json")).unwrap(),
    )
    .unwrap();
    assert!(report_with_keys.audit_continuous);
    assert!(report_with_keys.within_rto);
    assert!(report_with_keys.within_rpo);
    assert_eq!(report_with_keys.data_loss_records, 0);
    assert_eq!(report_with_keys.restored.keys.len(), 2);

    let _ = std::fs::remove_dir_all(&base_dir);
}
