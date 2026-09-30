use crate::task_lease::{CompleteVerdict, LeaseTable};

#[test]
fn vc_201_022_dispatch_assigns_expiring_ownership_and_fence() {
    let mut t = LeaseTable::new();
    let a = t.dispatch("n1", "worker-a", 100, 50);
    assert_eq!(a.owner, "worker-a");
    assert_eq!(a.fence, 1);
    assert_eq!(a.lease_until_unix, 150);
}

#[test]
fn vc_201_022_stale_fence_cannot_complete_after_reassign() {
    let mut t = LeaseTable::new();
    let old = t.dispatch("n1", "w1", 0, 100);
    let _new = t.dispatch("n1", "w2", 10, 100); // reassign bumps fence
    assert_eq!(
        t.complete("n1", "w1", old.fence, 20),
        CompleteVerdict::StaleFence
    );
    assert_eq!(t.complete("n1", "w2", 2, 20), CompleteVerdict::Accepted);
}

#[test]
fn vc_201_022_expired_or_wrong_owner_rejected() {
    let mut t = LeaseTable::new();
    let l = t.dispatch("n1", "w1", 0, 10);
    assert_eq!(
        t.complete("n1", "w1", l.fence, 10),
        CompleteVerdict::Expired
    );
    let l2 = t.dispatch("n1", "w1", 20, 10);
    assert_eq!(
        t.complete("n1", "other", l2.fence, 25),
        CompleteVerdict::NotOwner
    );
}

/// Wiring test: dispatch gates completion through the lease fence.
///
/// A node dispatched to a worker gets a lease with a monotonically
/// increasing fence. Completing with the wrong fence or a missing
/// lease is rejected; only the current owner with the live fence
/// succeeds. After acceptance the lease is removed so a second
/// complete cannot sneak through.
#[test]
fn task_lease_dispatch_wiring() {
    let mut t = LeaseTable::new();

    // Dispatch node 'n1' to worker-a at time 100 with TTL 50.
    let lease = t.dispatch("n1", "worker-a", 100, 50);
    assert_eq!(lease.fence, 1);

    // Wrong fence → rejected even for the correct owner.
    assert_eq!(
        t.complete("n1", "worker-a", lease.fence + 1, 110),
        CompleteVerdict::StaleFence
    );

    // Correct owner + correct fence → accepted and lease removed.
    assert_eq!(
        t.complete("n1", "worker-a", lease.fence, 110),
        CompleteVerdict::Accepted
    );

    // Lease gone: completing again is unknown task.
    assert_eq!(
        t.complete("n1", "worker-a", lease.fence, 120),
        CompleteVerdict::UnknownTask
    );

    // Dispatched to another worker: old owner rejected.
    let new_lease = t.dispatch("n2", "worker-b", 200, 100);
    assert_eq!(
        t.complete("n2", "worker-a", new_lease.fence, 250),
        CompleteVerdict::NotOwner
    );
    assert_eq!(
        t.complete("n2", "worker-b", new_lease.fence, 250),
        CompleteVerdict::Accepted
    );
}
