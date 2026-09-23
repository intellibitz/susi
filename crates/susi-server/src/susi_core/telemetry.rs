//! Host telemetry types for adaptive optimization and self-monitoring.
//!
//! These are pure domain types; OS-specific sampling lives in the daemon.

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
