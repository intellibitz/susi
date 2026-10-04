//! Mastery verification for VC-201-080: a scoped operator emergency stop
//! that persists, rejects new work, cancels eligible running tasks, enforces
//! stop at admission, scopes enforcement, and rejects tampered files.

use crate::emergency_stop::{EmergencyStopBus, RunningTask};
use crate::task_manager::{SwarmTaskManager, TaskStatus};
use std::sync::atomic::Ordering;

fn task(id: &str, cancellable: bool, external: bool, scope: &str) -> RunningTask {
    RunningTask::new(id, cancellable, external, scope)
}

/// Verification: the stop is enforced at admission. register_task consults the stop;
/// a task registered AFTER apply_stop is rejected at admission, not admitted to running,
/// and immediately marked cancelled if cancellable.
#[test]
fn vc_201_080_mastery_post_stop_registration_runs() {
    let mut bus = EmergencyStopBus::default();
    bus.apply_stop("fleet", "panic");
    assert!(bus.rejects_new_work());
    let err = bus.register_task(task("sneaky", true, false, "fleet"));
    assert!(
        err.is_err(),
        "task registered after stop must be rejected at admission"
    );
    assert!(
        !bus.is_task_running("sneaky"),
        "rejected task must not be running"
    );
    assert!(
        bus.status.cancelled_tasks.contains("sneaky"),
        "cancellable task cancelled at admission"
    );

    // A second apply_stop confirms it was never admitted as running work
    bus.apply_stop("fleet", "again");
    assert!(!bus.is_task_running("sneaky"));
}

/// Verification: scope is enforced, not decorative. A stop for 'region-a'
/// cancels tasks in region-a, rejects work for region-a, but leaves region-b
/// tasks running and allows region-b new work to be admitted.
#[test]
fn vc_201_080_mastery_scope_does_not_scope() {
    let mut bus = EmergencyStopBus::default();
    bus.register_task(task("region-b-task", true, false, "region-b"))
        .unwrap();
    bus.register_task(task("region-a-task", true, false, "region-a"))
        .unwrap();

    bus.apply_stop("region-a", "regional incident");

    // Scope isolation: region-a cancelled, region-b unaffected
    assert!(bus.status.cancelled_tasks.contains("region-a-task"));
    assert!(
        !bus.status.cancelled_tasks.contains("region-b-task"),
        "a region-a stop must not cancel region-b work"
    );

    // Rejection is scoped:
    assert!(bus.rejects_new_work_for_scope("region-a"));
    assert!(!bus.rejects_new_work_for_scope("region-b"));

    // Admission is scoped:
    assert!(
        bus.register_task(task("region-b-new", true, false, "region-b"))
            .is_ok(),
        "region-b work should be admitted under region-a stop"
    );
    assert!(
        bus.register_task(task("region-a-new", true, false, "region-a"))
            .is_err(),
        "region-a work must be rejected under region-a stop"
    );
}

/// Verification: persisted stop file is signed and tamper-evident.
/// Flipping `active: true` to `active: false` invalidates the signature
/// and causes restore_from to reject the tampered file.
#[test]
fn vc_201_080_mastery_tampered_file_lifts_the_stop() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("susi-estop-mastery-{nanos}.json"));
    let mut bus = EmergencyStopBus::default();
    bus.apply_stop("fleet", "panic");
    bus.persist_to(&path).unwrap();

    // Flip the flag — signature validation must detect tampering and fail.
    let raw = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"active\": true", "\"active\": false");
    std::fs::write(&path, raw).unwrap();

    let res = EmergencyStopBus::restore_from(&path);
    assert!(
        res.is_err(),
        "tampered file must be rejected by signature verification"
    );
    let _ = std::fs::remove_file(&path);
}

/// Verification: nominal behavior holds across registered cancellable tasks,
/// unreachable peers, unrecallable externals, and untampered persistence.
#[test]
fn vc_201_080_mastery_nominal_stop_holds() {
    let mut bus = EmergencyStopBus::default();
    bus.register_peer("p1", true);
    bus.register_peer("p2", false);
    bus.register_task(task("t1", true, false, "fleet")).unwrap();
    bus.register_task(task("ext-1", false, true, "fleet"))
        .unwrap();
    bus.apply_stop("fleet", "panic");
    assert!(bus.rejects_new_work());
    assert!(bus.status.cancelled_tasks.contains("t1"));
    assert!(bus.status.unreachable_peers.contains("p2"));
    assert!(bus.status.unrecalled_external.contains("ext-1"));

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("susi-estop-ok-{nanos}.json"));
    bus.persist_to(&path).unwrap();
    assert!(EmergencyStopBus::restore_from(&path)
        .unwrap()
        .rejects_new_work());
    let _ = std::fs::remove_file(&path);
}

/// Verification: EmergencyStopBus is wired into production task admission (SwarmTaskManager).
#[test]
fn vc_201_080_mastery_production_caller_wired() {
    let mgr = SwarmTaskManager::global();
    let test_scope = "isolated-estop-test-scope";

    // Before stop: task registers normally and runs
    let handle_before = mgr.register_task_scoped("job1", "test intent", Some(test_scope));
    assert_eq!(
        TaskStatus::from(handle_before.status.load(Ordering::Acquire)),
        TaskStatus::Running
    );
    assert!(!handle_before.is_cancelled());

    // Apply emergency stop to test_scope on global bus
    EmergencyStopBus::global()
        .write()
        .apply_stop(test_scope, "operator test halt");

    // After stop: registering task in stopped scope immediately marks it killed and cancelled
    let handle_after = mgr.register_task_scoped("job2", "test intent 2", Some(test_scope));
    assert!(
        handle_after.is_cancelled(),
        "task in stopped scope must start cancelled"
    );
    assert_eq!(
        TaskStatus::from(handle_after.status.load(Ordering::Acquire)),
        TaskStatus::Killed,
        "task in stopped scope must have status Killed"
    );

    // Clean up global stop
    EmergencyStopBus::global().write().status.stop = None;
}
