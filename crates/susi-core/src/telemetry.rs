//! Host telemetry types for adaptive optimization and self-monitoring.
//!
//! Domain types plus the one host sampler. Linux reads come from
//! `/sys/class/thermal`, `/sys/class/power_supply`, and `/proc/loadavg`;
//! other platforms return an empty snapshot.

use serde::{Deserialize, Serialize};

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
}

impl TelemetrySnapshot {
    pub fn empty() -> Self {
        Self {
            thermal_zones: Vec::new(),
            batteries: Vec::new(),
            load_avg_1m: None,
        }
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

/// Samples host telemetry directly from the kernel's `/sys` and `/proc`
/// interfaces.
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

#[cfg(target_os = "linux")]
fn read_load_avg_1m() -> Option<f32> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace()
        .next()
        .and_then(|s| s.parse::<f32>().ok())
}
