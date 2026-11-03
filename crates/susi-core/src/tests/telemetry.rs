//! Sampler tests — kept out of the canonical source so crates that
//! `#[path]`-mount `telemetry.rs` never compile or run them.

use crate::telemetry::sample;

#[test]
fn sample_does_not_panic() {
    let snap = sample();
    let _ = snap.max_temp_c();
    let _ = snap.critical_battery();
}

#[cfg(target_os = "linux")]
#[test]
fn sample_reads_real_load_avg_on_linux() {
    // /proc/loadavg is always present on Linux; a live host should
    // therefore never fall back to `None` here.
    assert!(sample().load_avg_1m.is_some());
}

#[test]
fn sample_reads_cpu_and_memory_on_every_supported_os() {
    let snap = sample();
    assert_eq!(snap.os, std::env::consts::OS);
    let cpu = snap.cpu_util_pct.expect("cpu utilization is sampled");
    assert!((0.0..=100.0).contains(&cpu), "{cpu}");
    let total = snap.mem_total_mb.expect("memory total is read");
    assert!(total > 0);
    assert!(snap.mem_available_mb.is_some_and(|a| a <= total));
    assert!(snap.has_readings());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn sample_names_linux_only_metrics_as_unavailable() {
    let snap = sample();
    assert!(snap.batteries.is_empty());
    assert!(
        snap.unavailable.contains_key("batteries"),
        "{:?}",
        snap.unavailable
    );
    if cfg!(windows) {
        assert!(snap.load_avg_1m.is_none());
        assert!(snap.unavailable.contains_key("load_avg_1m"));
    }
    // An empty sensor list is always explained, never silent.
    assert_eq!(
        snap.thermal_zones.is_empty(),
        snap.unavailable.contains_key("thermal_zones")
    );
    // The reasons survive the wire.
    let json = serde_json::to_value(&snap).unwrap();
    assert!(json["unavailable"]["batteries"].is_string());
}

#[test]
fn snapshots_without_the_new_fields_still_deserialize() {
    let old = serde_json::json!({
        "thermal_zones": [],
        "batteries": [],
        "load_avg_1m": 1.5
    });
    let snap: crate::telemetry::TelemetrySnapshot = serde_json::from_value(old).unwrap();
    assert_eq!(snap.load_avg_1m, Some(1.5));
    assert!(snap.unavailable.is_empty());
    assert!(snap.cpu_util_pct.is_none());
}
