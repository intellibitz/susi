//! Wiring: fair queues into concurrent MissionDag admits (T-INTELLIBITZ-18).

use crate::dag::MissionDag;
use crate::fair_queue::EnqueueResult;

#[test]
fn fair_queue_dispatch_wiring() {
    let mut dag = MissionDag::new("fair queue wiring");
    // Cap concurrency at default max_running (2).
    assert_eq!(dag.fair_admit("m1", "/ws-a", 0, 1), EnqueueResult::Running);
    assert_eq!(dag.fair_admit("m2", "/ws-b", 1, 1), EnqueueResult::Running);
    assert_eq!(dag.fair_admit("m3", "/ws-c", 2, 5), EnqueueResult::Queued);

    let promoted = dag.fair_complete("m1", 100).expect("promote");
    assert_eq!(promoted.mission_id, "m3");
    assert_eq!(dag.fair_queue.running_count(), 2);
    assert_eq!(dag.fair_queue.waiting_count(), 0);
}

#[test]
fn fair_queue_rejects_missions_when_waiting_queue_is_full() {
    let mut dag = MissionDag::new("waiting bound rejection");
    assert_eq!(dag.fair_admit("r0", "/ws-a", 0, 1), EnqueueResult::Running);
    assert_eq!(dag.fair_admit("r1", "/ws-b", 1, 1), EnqueueResult::Running);
    for idx in 0..8 {
        let id = format!("q{idx}");
        assert_eq!(
            dag.fair_admit(&id, "/ws-c", 2 + idx, 1),
            EnqueueResult::Queued
        );
    }
    assert_eq!(
        dag.fair_admit("overflow", "/ws-d", 20, 1),
        EnqueueResult::RejectedFull
    );
    assert_eq!(dag.fair_queue.running_count(), 2);
    assert_eq!(dag.fair_queue.waiting_count(), 8);
}

#[test]
fn fair_queue_complete_unknown_or_when_waiting_empty_returns_none() {
    let mut dag = MissionDag::new("empty waiting");
    assert_eq!(dag.fair_admit("m1", "/ws-a", 0, 1), EnqueueResult::Running);
    assert!(dag.fair_complete("unknown-id", 10).is_none());
    assert_eq!(dag.fair_queue.running_count(), 1);

    assert!(dag.fair_complete("m1", 20).is_none());
    assert_eq!(dag.fair_queue.running_count(), 0);
}

#[test]
fn fair_queue_promotes_by_weight_and_aging_boost() {
    let mut dag = MissionDag::new("weight and aging promotion");
    assert_eq!(dag.fair_admit("m1", "/ws-a", 0, 1), EnqueueResult::Running);
    assert_eq!(dag.fair_admit("m2", "/ws-a", 0, 1), EnqueueResult::Running);
    // Same enqueue timestamp (10): higher weight (50 > 1) wins before aging threshold.
    assert_eq!(dag.fair_admit("m3", "/ws-a", 10, 1), EnqueueResult::Queued);
    assert_eq!(dag.fair_admit("m4", "/ws-b", 10, 50), EnqueueResult::Queued);

    let first = dag.fair_complete("m1", 15).expect("promote higher weight");
    assert_eq!(first.mission_id, "m4");
    let second = dag
        .fair_complete("m2", 50)
        .expect("promote remaining waiter");
    assert_eq!(second.mission_id, "m3");
}

#[test]
fn fair_queue_starts_empty_with_zero_running_and_waiting_counts() {
    let mut dag = MissionDag::new("fresh fair queue");
    assert_eq!(dag.fair_queue.running_count(), 0);
    assert_eq!(dag.fair_queue.waiting_count(), 0);
    assert!(dag.fair_complete("nonexistent", 0).is_none());
}
