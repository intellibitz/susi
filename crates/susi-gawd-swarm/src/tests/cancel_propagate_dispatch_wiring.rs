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
