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
