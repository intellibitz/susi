//! Production-entry crash drills for VC-201-023.
//!
//! The tests drive `MissionDag`'s dispatch journal boundary, persist the
//! resulting mission, and rebuild the DAG as a fresh coordinator.  They use a
//! small in-memory external-effect counter so an acknowledged append/create/
//! deploy/patch cannot be mistaken for a second tool invocation.

use crate::dag::MissionDag;
use crate::mission_persist::PersistedMission;
use crate::side_effect_journal::DispatchDecision;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "susi-swarm-safe-replay-{tag}-{}-{suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn save_dag(dag: &MissionDag, dir: &Path) {
    dag.to_persisted("safe-replay")
        .save(dir)
        .expect("test mission should persist");
}

fn resume_dag(dir: &Path) -> MissionDag {
    let path = dir.join("safe-replay.json");
    let mission = PersistedMission::load(&path).expect("test mission should load");
    MissionDag::from_persisted(&mission)
}

fn count_effect(counter: &AtomicUsize) {
    counter.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn swarm_gap_safe_replay_crash_before_dispatch_stays_blocked_until_reconciled() {
    let dir = temp_dir("before-dispatch");
    let dag = MissionDag::new("append one record");
    let prepared = dag.prepare_dispatch_intent(
        0,
        "exec_command",
        "append record-1",
        Some("append:record-1"),
    );
    let intent_id = match prepared {
        DispatchDecision::Dispatch { intent_id } => intent_id,
        other => panic!("expected a new durable intent, got {other:?}"),
    };
    save_dag(&dag, &dir);

    let resumed = resume_dag(&dir);
    assert!(matches!(
        resumed.prepare_dispatch_intent(
            0,
            "exec_command",
            "append record-1",
            Some("append:record-1")
        ),
        DispatchDecision::ReconciliationRequired { .. }
    ));
    assert_eq!(
        resumed
            .intent_journal
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unreconciled("n0"),
        vec![intent_id]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn swarm_gap_safe_replay_crash_after_external_effect_before_receipt_reconciles_once() {
    let dir = temp_dir("before-receipt");
    let counter = AtomicUsize::new(0);
    let dag = MissionDag::new("deploy once");
    let intent_id = match dag.prepare_dispatch_intent(
        0,
        "exec_command",
        "deploy service-a",
        Some("deploy:service-a"),
    ) {
        DispatchDecision::Dispatch { intent_id } => intent_id,
        other => panic!("expected a new durable intent, got {other:?}"),
    };
    save_dag(&dag, &dir);

    // The provider applied the deployment, but the coordinator died before it
    // could publish the receipt.
    count_effect(&counter);
    let resumed = resume_dag(&dir);
    let blocked = resumed.prepare_dispatch_intent(
        0,
        "exec_command",
        "deploy service-a",
        Some("deploy:service-a"),
    );
    assert!(matches!(
        blocked,
        DispatchDecision::ReconciliationRequired { .. }
    ));

    let journal = resumed
        .intent_journal
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(journal.unreconciled("n0"), vec![intent_id.clone()]);
    drop(journal);
    resumed
        .intent_journal
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .reconcile_applied(&intent_id, "provider receipt", 22);
    save_dag(&resumed, &dir);

    let reconciled = resume_dag(&dir);
    assert!(matches!(
        reconciled.prepare_dispatch_intent(
            0,
            "exec_command",
            "deploy service-a",
            Some("deploy:service-a")
        ),
        DispatchDecision::AlreadyExecuted { .. }
    ));
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn swarm_gap_safe_replay_crash_after_receipt_skips_append_create_and_deploy() {
    for (operation, args) in [
        ("append:record-2", "append record-2"),
        ("create:resource-2", "create resource-2"),
        ("deploy:service-2", "deploy service-2"),
    ] {
        let dir = temp_dir(&operation.replace(':', "-"));
        let counter = AtomicUsize::new(0);
        let dag = MissionDag::new(args);
        let intent_id = match dag.prepare_dispatch_intent(0, "exec_command", args, Some(operation))
        {
            DispatchDecision::Dispatch { intent_id } => intent_id,
            other => panic!("expected a new durable intent, got {other:?}"),
        };
        save_dag(&dag, &dir);

        // External effect and receipt both happened before the coordinator
        // crashed, but node completion did not.
        count_effect(&counter);
        dag.mark_intent_executed_with_receipt(&intent_id, "provider receipt");
        save_dag(&dag, &dir);

        let resumed = resume_dag(&dir);
        assert!(matches!(
            resumed.prepare_dispatch_intent(0, "exec_command", args, Some(operation)),
            DispatchDecision::AlreadyExecuted { .. }
        ));
        assert_eq!(counter.load(Ordering::SeqCst), 1, "{operation}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn swarm_gap_safe_replay_patch_retries_only_after_not_applied_reconciliation() {
    let dir = temp_dir("patch");
    let counter = AtomicUsize::new(0);
    let dag = MissionDag::new("apply patch");
    let intent_id = match dag.prepare_dispatch_intent(
        0,
        "apply_patch",
        "patch file-a",
        Some("patch:file-a:v1"),
    ) {
        DispatchDecision::Dispatch { intent_id } => intent_id,
        other => panic!("expected a new durable intent, got {other:?}"),
    };
    save_dag(&dag, &dir);

    let resumed = resume_dag(&dir);
    assert!(matches!(
        resumed.prepare_dispatch_intent(0, "apply_patch", "patch file-a", Some("patch:file-a:v1")),
        DispatchDecision::ReconciliationRequired { .. }
    ));
    resumed
        .intent_journal
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .reconcile_not_applied(&intent_id);
    save_dag(&resumed, &dir);

    let retry = resume_dag(&dir);
    assert!(matches!(
        retry.prepare_dispatch_intent(0, "apply_patch", "patch file-a", Some("patch:file-a:v1")),
        DispatchDecision::Dispatch { .. }
    ));
    count_effect(&counter);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn swarm_gap_safe_replay_mutation_without_operation_key_is_refused() {
    let dag = MissionDag::new("unkeyed mutation");
    assert!(matches!(
        dag.prepare_dispatch_intent(0, "exec_command", "append record-3", None),
        DispatchDecision::ReconciliationRequired { reason, .. }
            if reason.contains("idempotency key")
    ));
}
