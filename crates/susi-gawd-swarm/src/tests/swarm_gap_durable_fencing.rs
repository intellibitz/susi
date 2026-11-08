//! Production-entry regressions for durable swarm authority (VC-201-022).
//!
//! These tests deliberately exercise the public mission/lease/isolation
//! boundaries rather than private JSON details.  Every test has the
//! `swarm_gap_durable_fencing_` prefix because a zero-test acceptance run must
//! never be mistaken for coverage of the restart and takeover gaps.

use crate::mission_persist::PersistedMission;
use crate::task_lease::{CompleteVerdict, LeaseTable, OwnershipEpoch};
use crate::writer_isolation::WriterIsolation;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "susi-swarm-gap-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn swarm_gap_durable_fencing_coordinator_restart_advances_authority() {
    let dir = temp_dir("restart");
    let mut mission = PersistedMission::new("restart-mission");
    let old_lease = mission
        .leases
        .dispatch("n0", "old-coordinator-worker", 10, 100);
    let old_token = old_lease.authority(mission.ownership);
    mission.set_active_scope("old-scope");
    mission.save(&dir).unwrap();

    let path = dir.join("restart-mission.json");
    let mut recovered = PersistedMission::load(&path).unwrap();
    let stale_scopes = recovered.begin_recovery("new-coordinator", 20);
    assert_eq!(stale_scopes, vec!["old-scope"]);
    assert!(recovered.ownership.mission > old_token.epoch.mission);
    assert!(recovered.ownership.coordinator > old_token.epoch.coordinator);
    assert_eq!(recovered.coordinator, "new-coordinator");
    assert_eq!(
        recovered
            .leases
            .check_authority("n0", "old-coordinator-worker", old_token, 21),
        CompleteVerdict::StaleFence
    );

    let replacement = recovered.leases.dispatch_in_epoch(
        "n0",
        "replacement-worker",
        21,
        100,
        recovered.ownership,
    );
    assert!(replacement.fence > old_lease.fence);
    assert_eq!(
        recovered.leases.check_authority(
            "n0",
            "replacement-worker",
            replacement.authority(recovered.ownership),
            22
        ),
        CompleteVerdict::Accepted
    );
}

#[test]
fn swarm_gap_durable_fencing_worker_takeover_rejects_delayed_write() {
    let mut leases = LeaseTable::new();
    let old = leases.dispatch("n0", "worker-old", 100, 300);
    let old_token = old.authority(leases.epoch);
    let delayed_permit = leases
        .authorize_mutation("n0", "worker-old", old_token, 101)
        .unwrap();
    let replacement = leases.dispatch("n0", "worker-new", 101, 300);
    let mut effects = Vec::new();

    assert_eq!(
        leases.check_permit(delayed_permit, 102),
        CompleteVerdict::StaleFence
    );
    if leases
        .authorize_mutation("n0", "worker-old", old_token, 102)
        .is_ok()
    {
        effects.push("old-worker".to_string());
    }
    assert!(
        effects.is_empty(),
        "stale worker must not reach the side effect"
    );

    if leases
        .authorize_mutation("n0", "worker-new", replacement.authority(leases.epoch), 102)
        .is_ok()
    {
        effects.push("replacement-worker".to_string());
    }
    assert_eq!(effects, vec!["replacement-worker"]);
}

#[test]
fn swarm_gap_durable_fencing_expired_lease_blocks_mutation_and_renewal() {
    let mut leases = LeaseTable::new();
    let lease = leases.dispatch("n0", "worker", 100, 1);
    let token = lease.authority(leases.epoch);
    assert_eq!(
        leases.authorize_mutation("n0", "worker", token, 101),
        Err(CompleteVerdict::Expired)
    );
    assert_eq!(
        leases.renew("n0", "worker", token, 101, 100),
        CompleteVerdict::Expired
    );
}

#[test]
fn swarm_gap_durable_fencing_duplicate_completion_is_not_a_second_write() {
    let mut leases = LeaseTable::new();
    let lease = leases.dispatch("n0", "worker", 100, 100);
    assert_eq!(
        leases.complete("n0", "worker", lease.fence, 101),
        CompleteVerdict::Accepted
    );
    assert_eq!(
        leases.complete("n0", "worker", lease.fence, 102),
        CompleteVerdict::UnknownTask
    );
}

#[test]
fn swarm_gap_durable_fencing_atomic_recovery_snapshot_roundtrips() {
    let dir = temp_dir("atomic");
    let mut mission = PersistedMission::new("atomic-mission");
    let before = mission.save(&dir).unwrap();
    let before_bytes = std::fs::read(&before).unwrap();
    let failed = mission.transaction(&dir, |candidate| {
        candidate.ownership = OwnershipEpoch {
            mission: 9,
            coordinator: 9,
        };
        Err(crate::susi_error::EaiError::governance("test rollback"))
    });
    assert!(failed.is_err());
    assert_eq!(std::fs::read(&before).unwrap(), before_bytes);

    let lease = mission.leases.dispatch("n0", "worker", 100, 100);
    mission.set_active_scope("scope-1");
    mission.save(&dir).unwrap();
    let token = lease.authority(mission.ownership);
    assert_eq!(
        mission
            .renew_lease(&dir, "n0", "worker", token, 110, 100,)
            .unwrap(),
        CompleteVerdict::Accepted
    );
    let loaded = PersistedMission::load(&before).unwrap();
    let renewed = loaded.leases.leases().get("n0").unwrap();
    assert_eq!(renewed.task_id, lease.task_id);
    assert_eq!(renewed.renewed_at_unix, 110);
    assert_eq!(loaded.leases.recovery.active_scopes.len(), 1);
    assert_eq!(loaded.recovery, loaded.leases.recovery);
}

#[test]
fn swarm_gap_durable_fencing_old_scope_is_quarantined_before_redispatch() {
    let dir = temp_dir("scope");
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    let isolation = WriterIsolation::new(&dir).unwrap();
    let old_scope = isolation.scope("worker-old-f1").unwrap();
    std::fs::write(old_scope.dir.join("old.txt"), "late write\n").unwrap();

    let retired = isolation.retire_stale_scopes("2-2").unwrap();
    assert_eq!(retired.len(), 1);
    assert!(!old_scope.dir.exists());
    std::fs::write(retired[0].join("delayed-old-write.txt"), "must not fold\n").unwrap();
    assert!(!dir.join("delayed-old-write.txt").exists());
    assert!(isolation.fold(&old_scope).is_err());
}
