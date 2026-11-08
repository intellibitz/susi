//! Durable fencing (T-DEVIN-9): the lease table and monotonic fence counter
//! ride mission state, the fence is checked BEFORE the first mutating
//! operation, and a stale worker is blocked from writing — not merely
//! rejected at completion when the damage is already done.

use crate::dag::MissionDag;
use crate::mission_persist::PersistedMission;
use crate::task_lease::{CompleteVerdict, LeaseTable};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "durable-fence-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn durable_fence_check_before_write_blocks_stale_worker() {
    let mut table = LeaseTable::new();
    let stale = table.dispatch("n0", "stale-worker", 100, 3600);
    let fresh = table.dispatch("n0", "fresh-worker", 100, 3600);
    assert_ne!(stale.fence, fresh.fence);

    // A stale worker checking its fence before mutating is refused; a live
    // lease-holder passes. The check consumes nothing.
    assert_eq!(
        table.check_fence("n0", "stale-worker", stale.fence, 101),
        CompleteVerdict::StaleFence
    );
    assert_eq!(
        table.check_fence("n0", "fresh-worker", fresh.fence, 101),
        CompleteVerdict::Accepted
    );
    // Idempotent: the read does not consume the lease.
    assert_eq!(
        table.check_fence("n0", "fresh-worker", fresh.fence, 101),
        CompleteVerdict::Accepted
    );
    assert_eq!(
        table.complete("n0", "fresh-worker", fresh.fence, 102),
        CompleteVerdict::Accepted
    );
    assert_eq!(
        table.check_fence("n0", "fresh-worker", fresh.fence, 103),
        CompleteVerdict::UnknownTask
    );
}

#[test]
fn durable_fence_expired_lease_refuses_writes_and_completion() {
    let mut table = LeaseTable::new();
    let lease = table.dispatch("n0", "w", 100, 10);
    assert_eq!(
        table.check_fence("n0", "w", lease.fence, 111),
        CompleteVerdict::Expired,
        "expired lease must refuse writes, not just completion"
    );
    assert_eq!(
        table.complete("n0", "w", lease.fence, 111),
        CompleteVerdict::Expired
    );
}

#[test]
fn durable_fence_state_roundtrips_through_mission_persist() {
    let dir = temp_dir("rt");
    let mut dag = MissionDag::new("fenced mission");
    let _ = dag.push_node("worker", "do work", vec![0]);
    let lease = dag.lease_dispatch(0, "worker-0", 100, 3_600);

    // Lease table rides the mission file.
    let persist = dag.to_persisted("m-fence");
    let path = persist.save(&dir).unwrap();
    let loaded = PersistedMission::load(&path).unwrap();
    assert_eq!(
        loaded
            .leases
            .check_fence("n0", "worker-0", lease.fence, 101),
        CompleteVerdict::Accepted,
        "persisted mission must retain the live lease + fence"
    );

    // A resumed run adopts the durable table: `next_fence` stays monotonic
    // so a crashed worker's token can never collide with a fresh lease.
    let mut dag2 = MissionDag::new("fenced mission");
    let _ = dag2.push_node("worker", "do work", vec![0]);
    dag2.leases.adopt(&loaded.leases);
    let reissue = dag2.lease_dispatch(0, "worker-0b", 101, 3_600);
    assert!(
        reissue.fence > lease.fence,
        "fence must advance past the persisted counter: {} vs {}",
        reissue.fence,
        lease.fence
    );
    // The stale worker's token is rejected — its writes would be discarded
    // instead of folded (gate in execute_dag_inner).
    assert_eq!(
        dag2.leases.check_fence("n0", "worker-0", lease.fence, 102),
        CompleteVerdict::StaleFence
    );
}

#[test]
fn durable_fence_adopt_never_rewinds_the_counter() {
    let mut older = LeaseTable::new();
    let mut newer = LeaseTable::new();
    for i in 0..3 {
        older.dispatch(&format!("t{i}"), "a", 0, 60);
    }
    newer.dispatch("t0", "b", 0, 60);
    newer.adopt(&older);
    let lease = newer.dispatch("t9", "b", 0, 60);
    assert_eq!(
        lease.fence, 4,
        "adopt must keep the highest seen fence, never rewind"
    );
}

#[test]
fn durable_fence_mission_file_is_stale_token_proof() {
    let dir = temp_dir("proof");
    let mut dag = MissionDag::new("m");
    let _ = dag.push_node("a", "x", vec![0]);
    let stale = dag.lease_dispatch(0, "old", 50, 3_600);
    let _fresh = dag.lease_dispatch(0, "new", 50, 3_600);

    let persist = dag.to_persisted("m-proof");
    persist.save(&dir).unwrap();
    let loaded = PersistedMission::load(&dir.join("m-proof.json")).unwrap();

    // After restart the stale fence is durable evidence, not lost memory.
    assert_eq!(
        loaded.leases.check_fence("n0", "old", stale.fence, 60),
        CompleteVerdict::StaleFence
    );
    assert_eq!(
        loaded.leases.check_fence("n0", "new", _fresh.fence, 60),
        CompleteVerdict::Accepted
    );
}
