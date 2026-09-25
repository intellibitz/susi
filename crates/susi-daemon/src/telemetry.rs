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

/// Samples host telemetry directly from the kernel's `/sys` and `/proc`
/// interfaces. Own copy rather than a `susi-gemi` dependency: the daemon
/// only needs these few read paths, not the rest of the inference-engine
/// crate, and `susi-daemon` already vendors small cross-cutting surfaces
/// (`susi_error`, `susi_config`, `susi_paths`) instead of pulling in their
/// owning crate.
pub fn sample() -> TelemetrySnapshot {
    #[cfg(target_os = "linux")]
    {
        TelemetrySnapshot {
            thermal_zones: read_thermal_zones(),
            batteries: read_batteries(),
            load_avg_1m: read_load_avg_1m(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        TelemetrySnapshot::empty()
    }
}

#[cfg(target_os = "linux")]
fn read_thermal_zones() -> Vec<susi_core::telemetry::ThermalZone> {
    use susi_core::telemetry::ThermalZone;

    let mut zones = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/thermal") else {
        return zones;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else {
            continue;
        };
        if !n.starts_with("thermal_zone") {
            continue;
        }
        let temp_path = entry.path().join("temp");
        let type_path = entry.path().join("type");
        if let Ok(text) = std::fs::read_to_string(&temp_path)
            && let Ok(mc) = text.trim().parse::<i64>()
        {
            let zone_type = std::fs::read_to_string(&type_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| n.to_string());
            zones.push(ThermalZone {
                name: format!("{n}:{zone_type}"),
                temp_mc: mc,
            });
        }
    }
    zones
}

#[cfg(target_os = "linux")]
fn read_batteries() -> Vec<susi_core::telemetry::BatteryInfo> {
    use susi_core::telemetry::BatteryInfo;

    let mut batteries = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return batteries;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(n) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let capacity =
            read_uevent_value(&path, "POWER_SUPPLY_CAPACITY").and_then(|s| s.parse::<u8>().ok());
        let status = read_uevent_value(&path, "POWER_SUPPLY_STATUS");
        let voltage = read_uevent_value(&path, "POWER_SUPPLY_VOLTAGE_NOW")
            .and_then(|s| s.parse::<f32>().ok())
            .map(|v| v / 1_000_000.0);
        let current = read_uevent_value(&path, "POWER_SUPPLY_CURRENT_NOW")
            .and_then(|s| s.parse::<f32>().ok())
            .map(|c| c / 1_000_000.0);
        let power_w = voltage.zip(current).map(|(v, c)| v * c);
        batteries.push(BatteryInfo {
            name: n.to_string(),
            capacity_pct: capacity,
            status,
            power_w,
        });
    }
    batteries
}

#[cfg(target_os = "linux")]
fn read_uevent_value(path: &std::path::Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path.join("uevent")).ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix(key)
            && let Some((_, v)) = value.split_once('=')
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_load_avg_1m() -> Option<f32> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace()
        .next()
        .and_then(|s| s.parse::<f32>().ok())
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

    #[cfg(target_os = "linux")]
    #[test]
    fn sample_reads_real_load_avg_on_linux() {
        // /proc/loadavg is always present on Linux; a live host should
        // therefore never fall back to `None` here.
        assert!(sample().load_avg_1m.is_some());
    }
}
