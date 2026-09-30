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
