//! Wiring: cancel propagation through MissionDag workers (T-INTELLIBITZ-9).

use crate::cancel_propagate::WorkerKind;
use crate::dag::MissionDag;

#[test]
fn cancel_propagate_dispatch_wiring() {
    let mut dag = MissionDag::new("cancel wiring");
    let _ = dag.push_node("peer", "peer work", vec![0]);

    dag.register_cancel_worker(0, WorkerKind::Local, true, 10_000);
    dag.register_cancel_worker(1, WorkerKind::PeerDispatch, false, 10_000);

    assert!(!dag.is_cancelled(100));
    let reports = dag.cancel_propagate();
    assert!(dag.is_cancelled(100));
    assert!(dag.cancel.terminated.contains("n0"));
    assert!(dag.cancel.irreversible_running.contains("n1"));
    assert!(
        reports.iter().any(|r| r.contains("n1")),
        "irreversible peer must be reported: {reports:?}"
    );
}

#[test]
fn cancel_propagate_all_reversible_worker_kinds_terminate_cleanly() {
    let mut dag = MissionDag::new("all reversible");
    let _ = dag.push_node("sub", "subprocess work", vec![0]);
    let _ = dag.push_node("peer", "peer work", vec![1]);

    dag.register_cancel_worker(0, WorkerKind::Local, true, 5_000);
    dag.register_cancel_worker(1, WorkerKind::ExternalAgent, true, 5_000);
    dag.register_cancel_worker(2, WorkerKind::PeerDispatch, true, 5_000);

    let reports = dag.cancel_propagate();
    assert!(
        reports.is_empty(),
        "all reversible workers should not produce reports: {reports:?}"
    );
    assert!(dag.cancel.irreversible_running.is_empty());
    assert!(dag.cancel.terminated.contains("n0"));
    assert!(dag.cancel.terminated.contains("n1"));
    assert!(dag.cancel.terminated.contains("n2"));
}

#[test]
fn cancel_propagate_deadline_expiration_trips_is_cancelled_without_explicit_propagate() {
    let mut dag = MissionDag::new("deadline expiry");
    dag.register_cancel_worker(0, WorkerKind::Local, true, 500);

    assert!(!dag.is_cancelled(499));
    assert!(dag.is_cancelled(500));
    assert!(dag.is_cancelled(1_000));
}

#[test]
fn cancel_propagate_is_idempotent_across_repeated_calls() {
    let mut dag = MissionDag::new("idempotent cancel");
    let _ = dag.push_node("peer", "irreversible peer", vec![0]);
    dag.register_cancel_worker(0, WorkerKind::Local, true, 10_000);
    dag.register_cancel_worker(1, WorkerKind::PeerDispatch, false, 10_000);

    let first = dag.cancel_propagate();
    let second = dag.cancel_propagate();
    assert_eq!(first, second);
    assert_eq!(dag.cancel.terminated.len(), 1);
    assert_eq!(dag.cancel.irreversible_running.len(), 1);
}

#[test]
fn cancel_propagate_on_dag_with_no_registered_workers_returns_empty_reports() {
    let mut dag = MissionDag::new("no registered workers");
    assert!(!dag.is_cancelled(0));
    let reports = dag.cancel_propagate();
    assert!(reports.is_empty());
    assert!(dag.cancel.terminated.is_empty());
    assert!(dag.cancel.irreversible_running.is_empty());
}
