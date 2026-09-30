//! Wiring: TaskLease fences on MissionDag completion (T-INTELLIBITZ-6 / VC-201-022).

use crate::dag::MissionDag;
use crate::task_lease::CompleteVerdict;

#[test]
fn task_lease_dispatch_wiring() {
    let mut dag = MissionDag::new("lease fence wiring");
    let _ = dag.push_node("child", "after root", vec![0]);

    // Dispatch assigns ownership + monotonic fence.
    let lease = dag.lease_dispatch(0, "worker-a", 100, 50);
    assert_eq!(lease.owner, "worker-a");
    assert_eq!(lease.fence, 1);
    assert_eq!(lease.lease_until_unix, 150);
    assert_eq!(lease.task_id, "n0");

    // Live fence completes the node.
    assert_eq!(
        dag.lease_complete(0, "worker-a", lease.fence, 120),
        CompleteVerdict::Accepted
    );
    assert!(dag.nodes[0].completed);

    // Re-dispatch child; a recovered worker with the old fence is refused.
    let old = dag.lease_dispatch(1, "worker-old", 200, 50);
    let live = dag.lease_dispatch(1, "worker-new", 210, 50);
    assert!(live.fence > old.fence);
    assert_eq!(
        dag.lease_complete(1, "worker-old", old.fence, 220),
        CompleteVerdict::StaleFence
    );
    assert!(
        !dag.nodes[1].completed,
        "stale fence must not mark the DAG node complete"
    );
    assert_eq!(
        dag.lease_complete(1, "worker-new", live.fence, 220),
        CompleteVerdict::Accepted
    );
    assert!(dag.nodes[1].completed);
}
