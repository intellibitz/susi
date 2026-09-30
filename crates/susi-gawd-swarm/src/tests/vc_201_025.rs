use crate::fair_queue::{EnqueueResult, FairQueue, QueueLimits, QueuedMission};

#[test]
fn vc_201_025_rejects_when_queue_full() {
    let mut q = FairQueue::new(QueueLimits {
        max_running: 1,
        max_queued: 1,
        age_boost_secs: 10,
    });
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "a".into(),
            workspace: "ws".into(),
            enqueued_at: 0,
            weight: 1,
        }),
        EnqueueResult::Running
    );
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "b".into(),
            workspace: "ws".into(),
            enqueued_at: 1,
            weight: 1,
        }),
        EnqueueResult::Queued
    );
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "c".into(),
            workspace: "ws".into(),
            enqueued_at: 2,
            weight: 1,
        }),
        EnqueueResult::RejectedFull
    );
}

#[test]
fn vc_201_025_aging_prevents_starvation() {
    let mut q = FairQueue::new(QueueLimits {
        max_running: 1,
        max_queued: 8,
        age_boost_secs: 30,
    });
    q.enqueue(QueuedMission {
        mission_id: "hot".into(),
        workspace: "ws".into(),
        enqueued_at: 0,
        weight: 10,
    });
    q.enqueue(QueuedMission {
        mission_id: "starved".into(),
        workspace: "ws".into(),
        enqueued_at: 1,
        weight: 1,
    });
    // Sustained load keeps re-enqueueing hot work after each completion,
    // but aged waiter must promote first.
    q.enqueue(QueuedMission {
        mission_id: "hot2".into(),
        workspace: "ws".into(),
        enqueued_at: 5,
        weight: 10,
    });
    assert!(q.would_starve_without_aging("hot", "starved", 50));
    let promoted = q.complete("hot", 50).expect("promote");
    assert_eq!(promoted.mission_id, "starved");
}

/// Wiring test: fair queues gate concurrent MissionDag admits.
///
/// Enforces the concurrency cap, queues up to the bound, then
/// rejects overflow; aging promotion lets an old waiter run
/// even when newer higher-weight work is queued.
#[test]
fn fair_queue_dispatch_wiring() {
    let mut q = FairQueue::new(QueueLimits {
        max_running: 2,
        max_queued: 2,
        age_boost_secs: 10,
    });
    // Under cap → running.
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "m1".into(),
            workspace: "ws".into(),
            enqueued_at: 0,
            weight: 5,
        }),
        EnqueueResult::Running
    );
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "m2".into(),
            workspace: "ws".into(),
            enqueued_at: 1,
            weight: 5,
        }),
        EnqueueResult::Running
    );
    // At bound → queued.
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "m3".into(),
            workspace: "ws".into(),
            enqueued_at: 2,
            weight: 5,
        }),
        EnqueueResult::Queued
    );
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "m4".into(),
            workspace: "ws".into(),
            enqueued_at: 3,
            weight: 5,
        }),
        EnqueueResult::Queued
    );
    // Overflow → rejected.
    assert_eq!(
        q.enqueue(QueuedMission {
            mission_id: "m5".into(),
            workspace: "ws".into(),
            enqueued_at: 4,
            weight: 5,
        }),
        EnqueueResult::RejectedFull
    );
    // Complete one running; aged waiter promotes.
    q.complete("m1", 20);
    let promoted = q.complete("m2", 20).expect("promote");
    assert!(matches!(promoted.mission_id.as_str(), "m3" | "m4"));
}
