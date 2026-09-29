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
