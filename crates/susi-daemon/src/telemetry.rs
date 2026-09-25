//! Real host telemetry sampler for adaptive optimization.
//!
//! Currently Linux-only (`/sys/class/thermal`, `/sys/class/power_supply`,
//! `/proc/loadavg`). Other platforms return an empty snapshot.

use std::path::Path;
use susi_core::context_graph::ContextGraph;
use susi_core::telemetry::TelemetrySnapshot;

/// Sample host telemetry and optionally record it into the context graph.
pub fn sample_and_record(workspace: Option<&Path>) -> TelemetrySnapshot {
    let snapshot = sample();
    if !snapshot.thermal_zones.is_empty()
        || !snapshot.batteries.is_empty()
        || snapshot.load_avg_1m.is_some()
    {
        ContextGraph::global().record_telemetry(&snapshot, workspace);
    }
    snapshot
}

/// Sample host telemetry via the GEMI hardware/telemetry layer.
pub fn sample() -> TelemetrySnapshot {
    // Vendored-type boundary: susi-gemi's TelemetrySnapshot is its vendored
    // copy's type; identical schema, so bridge through JSON.
    // let snap = susi_gemi::telemetry::sample();
    // Dummy snapshot for now, telemetry is handled by the microkernel
    let snap = serde_json::json!({});
    serde_json::to_string(&snap)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(TelemetrySnapshot::empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_does_not_panic() {
        let snap = sample();
        let _ = snap.max_temp_c();
        let _ = snap.critical_battery();
    }
}
