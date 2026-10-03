//! Mastery verification for VC-201-080: a scoped operator emergency stop
//! that persists, rejects new work, and cancels eligible running tasks.

use crate::emergency_stop::{EmergencyStopBus, RunningTask};

fn task(id: &str, cancellable: bool, external: bool) -> RunningTask {
    RunningTask {
        id: id.into(),
        cancellable,
        external,
    }
}

/// Falsification: the stop is not enforced at admission. register_task
/// never consults the stop — a task registered AFTER apply_stop sits in
/// the running set, uncancelled, while rejects_new_work claims the fleet
/// is stopped. 'Rejects new work' is a query nobody calls; the admission
/// point admits.
#[test]
fn vc_201_080_mastery_post_stop_registration_runs() {
    let mut bus = EmergencyStopBus::default();
    bus.apply_stop("fleet", "panic");
    assert!(bus.rejects_new_work());
    bus.register_task(task("sneaky", true, false));
    // The task is running — not cancelled, not rejected.
    assert!(!bus.status.cancelled_tasks.contains("sneaky"));
    // A second apply_stop reveals it was running all along.
    bus.apply_stop("fleet", "again");
    assert!(bus.status.cancelled_tasks.contains("sneaky"));
}

/// Falsification: 'scoped' is a stored label, not an enforcement. Tasks
/// carry no scope, so a stop for 'region-a' rejects work and cancels
/// tasks everywhere — the scope cannot exclude another region's work.
#[test]
fn vc_201_080_mastery_scope_does_not_scope() {
    let mut bus = EmergencyStopBus::default();
    bus.register_task(task("region-b-task", true, false));
    bus.apply_stop("region-a", "regional incident");
    assert!(
        bus.status.cancelled_tasks.contains("region-b-task"),
        "a region-a stop cancelled region-b work — scope is decorative"
    );
    // rejects_new_work is global: region-b's new work is refused too.
    assert!(bus.rejects_new_work());
}

/// Falsification: the persisted stop file is unsigned JSON — editing
/// `active` to false lifts the stop silently across restart. A durability
/// mechanism that trusts its own file is not durable against tampering.
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
    // Flip the flag — nothing detects it.
    let raw = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"active\": true", "\"active\": false");
    std::fs::write(&path, raw).unwrap();
    let restored = EmergencyStopBus::restore_from(&path).unwrap();
    assert!(
        !restored.rejects_new_work(),
        "tampered file lifted the stop"
    );
    let _ = std::fs::remove_file(&path);
}

/// What holds: registered cancellable tasks are cancelled, unreachable
/// peers and unrecallable externals are reported, and a non-tampered
/// persisted stop survives restart.
#[test]
fn vc_201_080_mastery_nominal_stop_holds() {
    let mut bus = EmergencyStopBus::default();
    bus.register_peer("p1", true);
    bus.register_peer("p2", false);
    bus.register_task(task("t1", true, false));
    bus.register_task(task("ext-1", false, true));
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
