use crate::emergency_stop::{EmergencyStopBus, RunningTask};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn vc_201_080_stop_rejects_new_work_and_cancels_eligible() {
    let mut bus = EmergencyStopBus::default();
    bus.register_peer("p1", true);
    bus.register_peer("p2", false);
    bus.register_task(RunningTask::new("t1", true, false, "fleet"))
        .unwrap();
    bus.register_task(RunningTask::new("ext-1", false, true, "fleet"))
        .unwrap();
    assert!(!bus.rejects_new_work());
    bus.apply_stop("fleet", "operator panic");
    assert!(bus.rejects_new_work());
    assert!(bus.status.cancelled_tasks.contains("t1"));
    assert!(bus.status.unreachable_peers.contains("p2"));
    assert!(bus.status.unrecalled_external.contains("ext-1"));
}

#[test]
fn vc_201_080_persists_across_restart() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("susi-estop-{nanos}.json"));
    let mut bus = EmergencyStopBus::default();
    bus.apply_stop("region-a", "drill");
    bus.persist_to(&path).unwrap();
    let restored = EmergencyStopBus::restore_from(&path).unwrap();
    assert!(restored.rejects_new_work());
    assert_eq!(restored.status.stop.as_ref().unwrap().scope, "region-a");
    let _ = std::fs::remove_file(&path);
}
