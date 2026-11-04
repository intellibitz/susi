use crate::coordinator_fence::{FencedMissionStore, LeadershipEpoch, MissionWrite, WriteVerdict};

#[test]
fn vc_201_035_rejects_stale_coordinator_after_failover() {
    let mut store = FencedMissionStore::new();
    assert!(store.accept_epoch(LeadershipEpoch {
        epoch: 1,
        coordinator: "c1".into(),
    }));
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m".into(),
            epoch: 1,
            coordinator: "c1".into(),
            payload: "v1".into(),
        }),
        WriteVerdict::Accepted
    );
    // Failover to c2.
    assert!(store.accept_epoch(LeadershipEpoch {
        epoch: 2,
        coordinator: "c2".into(),
    }));
    // Partitioned c1 write rejected.
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m".into(),
            epoch: 1,
            coordinator: "c1".into(),
            payload: "stale".into(),
        }),
        WriteVerdict::StaleEpoch
    );
    assert_eq!(store.mission("m").map(String::as_str), Some("v1"));
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m".into(),
            epoch: 2,
            coordinator: "c2".into(),
            payload: "v2".into(),
        }),
        WriteVerdict::Accepted
    );
    assert_eq!(store.mission("m").map(String::as_str), Some("v2"));
}

#[test]
fn vc_201_035_wrong_coordinator_same_epoch_rejected() {
    let mut store = FencedMissionStore::new();
    store.accept_epoch(LeadershipEpoch {
        epoch: 5,
        coordinator: "lead".into(),
    });
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m".into(),
            epoch: 5,
            coordinator: "imposter".into(),
            payload: "x".into(),
        }),
        WriteVerdict::WrongCoordinator
    );
}

#[test]
fn vc_201_035_accept_epoch_rejects_lower_or_equal_epoch() {
    let mut store = FencedMissionStore::new();
    assert!(store.accept_epoch(LeadershipEpoch {
        epoch: 4,
        coordinator: "c4".into(),
    }));
    assert!(!store.accept_epoch(LeadershipEpoch {
        epoch: 4,
        coordinator: "c4-dup".into(),
    }));
    assert!(!store.accept_epoch(LeadershipEpoch {
        epoch: 2,
        coordinator: "c2-old".into(),
    }));
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m".into(),
            epoch: 4,
            coordinator: "c4".into(),
            payload: "ok".into(),
        }),
        WriteVerdict::Accepted
    );
    assert_eq!(store.mission("m").map(String::as_str), Some("ok"));
}

#[test]
fn vc_201_035_independent_missions_update_without_cross_talk() {
    let mut store = FencedMissionStore::new();
    assert!(store.accept_epoch(LeadershipEpoch {
        epoch: 1,
        coordinator: "lead".into(),
    }));
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m1".into(),
            epoch: 1,
            coordinator: "lead".into(),
            payload: "alpha".into(),
        }),
        WriteVerdict::Accepted
    );
    assert_eq!(
        store.write(MissionWrite {
            mission_id: "m2".into(),
            epoch: 1,
            coordinator: "lead".into(),
            payload: "beta".into(),
        }),
        WriteVerdict::Accepted
    );
    assert_eq!(store.mission("m1").map(String::as_str), Some("alpha"));
    assert_eq!(store.mission("m2").map(String::as_str), Some("beta"));
    assert!(store.mission("m3").is_none());
}

#[test]
fn vc_201_035_uninitialized_store_rejects_writes_before_first_epoch() {
    let mut store = FencedMissionStore::new();
    let verdict = store.write(MissionWrite {
        mission_id: "m0".into(),
        epoch: 1,
        coordinator: "c1".into(),
        payload: "unannounced".into(),
    });
    assert_ne!(verdict, WriteVerdict::Accepted);
    assert!(store.mission("m0").is_none());
}
