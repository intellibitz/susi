//! Host telemetry types for adaptive optimization and self-monitoring.
//!
//! Domain types plus the one host sampler. Linux reads thermal zones from
//! `/sys/class/thermal` and batteries from `/sys/class/power_supply`; load,
//! CPU utilization and memory come from `susi_config::host_metrics` (the
//! vendored `sysinfo` facade) on every OS, and macOS/Windows temperatures
//! from the sensors that facade can read. A metric this OS cannot provide is
//! named in [`TelemetrySnapshot::unavailable`] with the reason, never
//! reported as an empty reading that looks like "nothing to report".

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One thermal zone reading (typically millidegrees Celsius).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalZone {
    pub name: String,
    pub temp_mc: i64,
}

/// Battery state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryInfo {
    pub name: String,
    pub capacity_pct: Option<u8>,
    pub status: Option<String>,
    /// Instantaneous power in watts (positive = charging, negative = discharging).
    pub power_w: Option<f32>,
}

/// A host telemetry snapshot used by adaptive optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub thermal_zones: Vec<ThermalZone>,
    pub batteries: Vec<BatteryInfo>,
    pub load_avg_1m: Option<f32>,
    /// Target OS the snapshot was taken on (`linux`, `windows`, `macos`).
    #[serde(default)]
    pub os: String,
    /// Host-wide CPU utilization in percent, sampled since the previous
    /// snapshot in this process (Windows has no load average, so this is
    /// its CPU stress signal).
    #[serde(default)]
    pub cpu_util_pct: Option<f32>,
    /// Installed physical memory in MiB.
    #[serde(default)]
    pub mem_total_mb: Option<u64>,
    /// Memory available to new work in MiB.
    #[serde(default)]
    pub mem_available_mb: Option<u64>,
    /// Metric name → why this OS cannot provide it. Present so an empty
    /// `batteries` or a missing `load_avg_1m` is never mistaken for a real
    /// "none" reading.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unavailable: BTreeMap<String, String>,
}

impl TelemetrySnapshot {
    pub fn empty() -> Self {
        Self {
            thermal_zones: Vec::new(),
            batteries: Vec::new(),
            load_avg_1m: None,
            os: std::env::consts::OS.to_string(),
            cpu_util_pct: None,
            mem_total_mb: None,
            mem_available_mb: None,
            unavailable: BTreeMap::new(),
        }
    }

    /// True when the snapshot carries at least one real reading.
    pub fn has_readings(&self) -> bool {
        !self.thermal_zones.is_empty()
            || !self.batteries.is_empty()
            || self.load_avg_1m.is_some()
            || self.cpu_util_pct.is_some()
            || self.mem_total_mb.is_some()
    }

    /// Highest thermal zone temperature in degrees Celsius, if any.
    pub fn max_temp_c(&self) -> Option<f32> {
        self.thermal_zones
            .iter()
            .map(|z| z.temp_mc as f32 / 1000.0)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// True if any battery is discharging and below 20%.
    pub fn critical_battery(&self) -> bool {
        self.batteries.iter().any(|b| {
            b.status
                .as_ref()
                .is_some_and(|s| s.eq_ignore_ascii_case("Discharging"))
                && b.capacity_pct.is_some_and(|c| c < 20)
        })
    }
}

/// Samples host telemetry: thermal zones and batteries from the kernel's
/// `/sys` interfaces on Linux, sensors through `sysinfo` elsewhere, and
/// load, CPU and memory through `susi_config::host_metrics` everywhere.
pub fn sample() -> TelemetrySnapshot {
    use crate::susi_config::host_metrics;
    let os = std::env::consts::OS;
    let mut missing: BTreeMap<String, String> = BTreeMap::new();
    let mut unavailable = |metric: &str, reason: String| {
        missing.insert(metric.to_string(), reason);
    };
    let thermal_zones = read_thermal_zones();
    if thermal_zones.is_empty() {
        unavailable(
            "thermal_zones",
            if cfg!(target_os = "linux") {
                "no readable zone under /sys/class/thermal on this host".to_string()
            } else {
                format!("no temperature sensor is readable on {os} by this process")
            },
        );
    }
    let batteries = read_batteries();
    if !cfg!(target_os = "linux") {
        unavailable(
            "batteries",
            format!("battery telemetry is Linux-only (/sys/class/power_supply); not read on {os}"),
        );
    }
    let load_avg_1m = host_metrics::load_average().map(|[one, _, _]| one as f32);
    if let Some(reason) = host_metrics::load_average_unavailable() {
        unavailable("load_avg_1m", reason.to_string());
    }
    let cpu_util_pct = host_metrics::cpu_util_pct();
    if cpu_util_pct.is_none() {
        unavailable("cpu_util_pct", format!("{os} reports no CPU utilization"));
    }
    let memory = host_metrics::memory();
    if memory.is_none() {
        unavailable("memory", format!("{os} reports no memory totals"));
    }
    TelemetrySnapshot {
        thermal_zones,
        batteries,
        load_avg_1m,
        os: os.to_string(),
        cpu_util_pct,
        mem_total_mb: memory.map(|m| m.total_bytes / (1024 * 1024)),
        mem_available_mb: memory.map(|m| m.available_bytes / (1024 * 1024)),
        unavailable: missing,
    }
}

/// Temperatures through the `sysinfo` sensors where there is no
/// `/sys/class/thermal` (macOS, Windows).
#[cfg(not(target_os = "linux"))]
fn read_thermal_zones() -> Vec<ThermalZone> {
    crate::susi_config::host_metrics::temperature_sensors()
        .into_iter()
        .map(|(name, celsius)| ThermalZone {
            name,
            temp_mc: (f64::from(celsius) * 1000.0).round() as i64,
        })
        .collect()
}

/// Batteries are read from `/sys/class/power_supply` only; elsewhere the
/// snapshot names them unavailable.
#[cfg(not(target_os = "linux"))]
fn read_batteries() -> Vec<BatteryInfo> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn read_thermal_zones() -> Vec<ThermalZone> {
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
        if let Ok(text) = std::fs::read_to_string(&temp_path) {
            if let Ok(mc) = text.trim().parse::<i64>() {
                let zone_type = std::fs::read_to_string(&type_path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| n.to_string());
                zones.push(ThermalZone {
                    name: format!("{n}:{zone_type}"),
                    temp_mc: mc,
                });
            }
        }
    }
    zones
}

#[cfg(target_os = "linux")]
fn read_batteries() -> Vec<BatteryInfo> {
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
        if let Some(value) = line.strip_prefix(key) {
            if let Some((_, v)) = value.split_once('=') {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_no_readings_and_matches_target_os() {
        let snap = TelemetrySnapshot::empty();
        assert!(!snap.has_readings());
        assert_eq!(snap.os, std::env::consts::OS);
        assert_eq!(snap.max_temp_c(), None);
        assert!(!snap.critical_battery());
    }

    #[test]
    fn max_temp_c_converts_millidegrees_and_picks_highest_zone() {
        let mut snap = TelemetrySnapshot::empty();
        snap.thermal_zones.push(ThermalZone {
            name: "cpu0".to_string(),
            temp_mc: 45_500,
        });
        snap.thermal_zones.push(ThermalZone {
            name: "gpu0".to_string(),
            temp_mc: 72_250,
        });
        assert!(snap.has_readings());
        let max = snap.max_temp_c().expect("max temp");
        assert!((max - 72.25).abs() < 1e-3);
    }

    #[test]
    fn critical_battery_detects_discharging_below_twenty_percent() {
        let mut snap = TelemetrySnapshot::empty();
        snap.batteries.push(BatteryInfo {
            name: "BAT0".to_string(),
            capacity_pct: Some(19),
            status: Some("discharging".to_string()),
            power_w: Some(-12.5),
        });
        assert!(snap.critical_battery());

        snap.batteries[0].status = Some("Charging".to_string());
        assert!(!snap.critical_battery());

        snap.batteries[0].status = Some("Discharging".to_string());
        snap.batteries[0].capacity_pct = Some(20);
        assert!(!snap.critical_battery());
    }

    #[test]
    fn sample_populates_os_and_records_readings_or_unavailable_reasons() {
        let snap = sample();
        assert_eq!(snap.os, std::env::consts::OS);
        assert!(snap.has_readings() || !snap.unavailable.is_empty());
        if snap.thermal_zones.is_empty() {
            assert!(snap.unavailable.contains_key("thermal_zones"));
        }
    }
}
