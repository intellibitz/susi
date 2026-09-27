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
