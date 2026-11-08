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

#[test]
fn task_lease_dispatch_wiring_rejects_expired_wrong_owner_and_unknown_task() {
    let mut dag = MissionDag::new("lease rejection modes");
    assert_eq!(
        dag.lease_complete(0, "worker-a", 1, 100),
        CompleteVerdict::UnknownTask
    );

    let lease = dag.lease_dispatch(0, "worker-a", 100, 30);
    assert_eq!(lease.lease_until_unix, 130);
    assert_eq!(
        dag.lease_complete(0, "worker-b", lease.fence, 110),
        CompleteVerdict::NotOwner
    );
    assert_eq!(
        dag.lease_complete(0, "worker-a", lease.fence, 130),
        CompleteVerdict::Expired
    );
    assert!(!dag.nodes[0].completed);
}

#[test]
fn task_lease_dispatch_wiring_authority_token_and_epoch_advance_invalidate_prior_lease() {
    let mut dag = MissionDag::new("epoch fencing");
    let _lease = dag.lease_dispatch(0, "worker-a", 100, 50);
    let token = dag.lease_authority(0).expect("authority token");

    assert_eq!(
        dag.check_mutation_authority(0, "worker-a", token, 110),
        CompleteVerdict::Accepted
    );
    assert_eq!(
        dag.check_mutation_authority(0, "worker-b", token, 110),
        CompleteVerdict::NotOwner
    );

    dag.ownership.coordinator += 1;
    dag.leases.epoch = dag.ownership;
    assert_eq!(
        dag.check_mutation_authority(0, "worker-a", token, 110),
        CompleteVerdict::StaleFence
    );
}

#[test]
fn task_lease_dispatch_wiring_persisted_recovery_preserves_monotonic_fences() {
    let mut dag = MissionDag::new("persisted fence monotonicity");
    let first = dag.lease_dispatch(0, "worker-1", 100, 30);
    let second = dag.lease_dispatch(0, "worker-2", 110, 30);
    assert_eq!((first.fence, second.fence), (1, 2));

    let persisted = dag.to_persisted("mission-fences");
    let mut recovered = MissionDag::from_persisted(&persisted);
    let third = recovered.lease_dispatch(0, "worker-3", 200, 30);
    assert!(
        third.fence > second.fence,
        "recovered lease fence {} must exceed pre-crash fence {}",
        third.fence,
        second.fence
    );
    assert_eq!(
        recovered.lease_complete(0, "worker-2", second.fence, 120),
        CompleteVerdict::StaleFence
    );
}

#[test]
fn task_lease_dispatch_wiring_lease_authority_is_none_before_first_dispatch() {
    let dag = MissionDag::new("undispatched authority");
    assert!(dag.lease_authority(0).is_none());
    assert!(dag.lease_authority(99).is_none());
}
