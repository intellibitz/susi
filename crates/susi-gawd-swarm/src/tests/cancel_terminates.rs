//! Cancellation must terminate live workers, not just mark ids (T-DEVIN-8):
//! propagate sets worker signals, cancels the mission's task-registry scope
//! (killing in-flight `exec_command` children), and a cancelled scope refuses
//! new work — nothing keeps mutating or consuming resources after cancel.

use crate::cancel_propagate::{CancelBus, CancelToken, Descendant, WorkerKind};
use crate::dag::MissionDag;
use crate::susi_core::task_manager::SwarmTaskManager;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn fresh_scope(tag: &str) -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "cancel-terminates-test-{}-{}",
        tag,
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[test]
fn cancel_terminates_propagate_kills_scoped_running_task() {
    let scope = fresh_scope("prop");
    let mgr = SwarmTaskManager::global();
    let handle = mgr.register_task_scoped("exec_command", "sleep 600", Some(&scope));
    assert!(!handle.is_cancelled(), "pre-condition: task running");

    let signal = Arc::new(AtomicBool::new(false));
    let mut bus = CancelBus::default();
    bus.set_token(CancelToken::fresh("m", CancelBus::now_unix() + 60));
    bus.register(Descendant {
        id: "n0".into(),
        kind: WorkerKind::Local,
        cancellable: true,
        cancel_scope: Some(scope.clone()),
        signal: Some(Arc::clone(&signal)),
    });

    let reports = bus.propagate();
    assert!(reports.is_empty(), "{reports:?}");
    assert!(bus.terminated.contains("n0"));
    // Real termination, not bookkeeping: the worker's signal fired and its
    // task flag is set, so the exec poll loop kills the child process.
    assert!(signal.load(Ordering::Acquire));
    assert!(handle.is_cancelled());
    assert!(mgr.is_scope_cancelled(&scope));
}

#[test]
fn cancel_terminates_scope_is_sticky_for_late_tasks() {
    let scope = fresh_scope("sticky");
    let mgr = SwarmTaskManager::global();
    assert_eq!(mgr.cancel_scope(&scope), 0, "nothing running yet");

    // A worker that wakes up after cancel must not start new commands.
    let late = mgr.register_task_scoped("exec_command", "rm -rf .", Some(&scope));
    assert!(
        late.is_cancelled(),
        "task registered under a cancelled scope must start cancelled"
    );

    // Unrelated scopes are untouched.
    let other_scope = fresh_scope("other");
    let other = mgr.register_task_scoped("exec_command", "echo ok", Some(&other_scope));
    assert!(!other.is_cancelled());
}

#[test]
fn cancel_terminates_only_the_named_scope() {
    let mgr = SwarmTaskManager::global();
    let doomed = fresh_scope("doomed");
    let spared = fresh_scope("spared");
    let a = mgr.register_task_scoped("exec_command", "sleep 1", Some(&doomed));
    let b = mgr.register_task_scoped("exec_command", "sleep 1", Some(&spared));
    let c = mgr.register_task("exec_command", "sleep 1");

    assert_eq!(mgr.cancel_scope(&doomed), 1);
    assert!(a.is_cancelled());
    assert!(!b.is_cancelled(), "other scope must survive");
    assert!(!c.is_cancelled(), "unscoped task must survive");

    a.mark_failed("cancelled");
    b.mark_completed("ok");
    c.mark_completed("ok");
}

#[test]
fn cancel_terminates_dag_workers_via_scoped_registration() {
    let mut dag = MissionDag::new("cancel mission");
    let idx = dag.push_node("work", "do work", vec![0]);
    let scope = fresh_scope("dag");
    let signal = Arc::new(AtomicBool::new(false));
    let mgr = SwarmTaskManager::global();
    let running = mgr.register_task_scoped("exec_command", "sleep 9", Some(&scope));

    dag.register_cancel_worker_scoped(
        idx,
        CancelBus::now_unix() + 60,
        Descendant {
            id: String::new(),
            kind: WorkerKind::Local,
            cancellable: true,
            cancel_scope: Some(scope.clone()),
            signal: Some(Arc::clone(&signal)),
        },
    );

    let _ = dag.cancel_propagate();
    assert!(dag.is_cancelled(CancelBus::now_unix()));
    assert!(signal.load(Ordering::Acquire));
    assert!(running.is_cancelled(), "live worker task must be killed");
    assert!(mgr.is_scope_cancelled(&scope));
    assert!(dag.cancel.terminated.contains("n1"));
}

#[test]
fn cancel_terminates_uncancellable_workers_survive() {
    let scope = fresh_scope("uncancellable");
    let mgr = SwarmTaskManager::global();
    let peer_work = mgr.register_task_scoped("peer_dispatch", "remote job", Some(&scope));

    let mut bus = CancelBus::default();
    bus.set_token(CancelToken::fresh("m", CancelBus::now_unix() + 60));
    bus.register(Descendant {
        id: "peer-1".into(),
        kind: WorkerKind::PeerDispatch,
        cancellable: false,
        cancel_scope: Some(scope.clone()),
        signal: None,
    });

    let reports = bus.propagate();
    assert!(reports.iter().any(|r| r.contains("peer-1")));
    // Not cancellable — its scope is NOT killed; honest irreversible report.
    assert!(!peer_work.is_cancelled());
    assert!(!mgr.is_scope_cancelled(&scope));
    peer_work.mark_failed("done");
}
