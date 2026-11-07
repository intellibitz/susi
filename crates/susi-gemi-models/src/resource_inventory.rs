//! Observed local AI resource inventory (VC-201-041).
//!
//! Unifies CPU, RAM, GPU, VRAM, disk, model, and runtime observations with
//! timestamps and provenance. Unavailable probes are [`Observation::Unknown`]
//! — never fabricated zeros or fake readiness.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How a value was obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeProvenance {
    /// Read from a live host probe (sysfs, /proc, nvidia-smi wrapper, …).
    Observed { source: String },
    /// Explicitly unavailable on this host/build.
    Unavailable { reason: String },
}

/// A numeric or string observation that may be unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation<T> {
    Value {
        value: T,
        observed_unix: u64,
        provenance: ProbeProvenance,
    },
    Unknown {
        observed_unix: u64,
        provenance: ProbeProvenance,
    },
}

impl<T> Observation<T> {
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown { .. })
    }

    #[must_use]
    pub fn value_ref(&self) -> Option<&T> {
        match self {
            Self::Value { value, .. } => Some(value),
            Self::Unknown { .. } => None,
        }
    }
}

/// Explicit probe results used to assemble an inventory (testable).
#[derive(Debug, Clone, Default)]
pub struct ProbeInputs {
    pub cpu_cores: Option<u32>,
    pub ram_bytes: Option<u64>,
    pub gpu_name: Option<String>,
    pub vram_bytes: Option<u64>,
    pub disk_free_bytes: Option<u64>,
    pub models: Option<Vec<String>>,
    pub runtimes: Option<Vec<String>>,
    pub now_unix: Option<u64>,
}

/// Unified inventory snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceInventory {
    pub captured_unix: u64,
    pub cpu_cores: Observation<u32>,
    pub ram_bytes: Observation<u64>,
    pub gpu_name: Observation<String>,
    pub vram_bytes: Observation<u64>,
    pub disk_free_bytes: Observation<u64>,
    pub models: Observation<Vec<String>>,
    pub runtimes: Observation<Vec<String>>,
}

impl ResourceInventory {
    /// Build an inventory from explicit probe results (testable without host I/O).
    #[must_use]
    pub fn from_probes(inputs: ProbeInputs) -> Self {
        let t = inputs.now_unix.unwrap_or_else(now_unix);
        Self {
            captured_unix: t,
            cpu_cores: observe(inputs.cpu_cores, t, "cpu", "cpu count unavailable"),
            ram_bytes: observe(inputs.ram_bytes, t, "meminfo", "ram unavailable"),
            gpu_name: observe(inputs.gpu_name, t, "gpu", "no GPU probe"),
            vram_bytes: observe(inputs.vram_bytes, t, "vram", "vram unavailable"),
            disk_free_bytes: observe(
                inputs.disk_free_bytes,
                t,
                "statvfs",
                "disk free unavailable",
            ),
            models: observe(inputs.models, t, "model_scan", "model scan not run"),
            runtimes: observe(inputs.runtimes, t, "runtime_scan", "runtime scan not run"),
        }
    }

    /// True when any critical readiness field is unknown — never treat as ready.
    #[must_use]
    pub fn has_unknown_readiness(&self) -> bool {
        self.cpu_cores.is_unknown()
            || self.ram_bytes.is_unknown()
            || self.vram_bytes.is_unknown()
            || self.disk_free_bytes.is_unknown()
    }
}

fn observe<T>(v: Option<T>, observed_unix: u64, source: &str, reason: &str) -> Observation<T> {
    match v {
        Some(value) => Observation::Value {
            value,
            observed_unix,
            provenance: ProbeProvenance::Observed {
                source: source.into(),
            },
        },
        None => Observation::Unknown {
            observed_unix,
            provenance: ProbeProvenance::Unavailable {
                reason: reason.into(),
            },
        },
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn observation_helpers_distinguish_value_and_unknown_variants() {
        let val = observe(Some(16_u32), 1_700_000_000, "cpu", "missing");
        assert!(!val.is_unknown());
        assert_eq!(val.value_ref(), Some(&16));

        let unk = observe::<u32>(None, 1_700_000_000, "cpu", "cpu count unavailable");
        assert!(unk.is_unknown());
        assert_eq!(unk.value_ref(), None);
    }

    #[test]
    fn from_probes_populates_all_observed_fields_and_clears_unknown_readiness() {
        let inv = ResourceInventory::from_probes(ProbeInputs {
            cpu_cores: Some(8),
            ram_bytes: Some(32 * 1024 * 1024 * 1024),
            gpu_name: Some("RTX 4090".to_string()),
            vram_bytes: Some(24 * 1024 * 1024 * 1024),
            disk_free_bytes: Some(500 * 1024 * 1024 * 1024),
            models: Some(vec!["qwen2.5-7b".to_string()]),
            runtimes: Some(vec!["candle-cpu".to_string()]),
            now_unix: Some(1_700_000_123),
        });
        assert_eq!(inv.captured_unix, 1_700_000_123);
        assert!(!inv.has_unknown_readiness());
        assert_eq!(inv.cpu_cores.value_ref(), Some(&8));
        assert_eq!(
            inv.gpu_name.value_ref().map(String::as_str),
            Some("RTX 4090")
        );
    }

    #[test]
    fn has_unknown_readiness_triggers_when_any_critical_probe_is_missing() {
        let base = ProbeInputs {
            cpu_cores: Some(8),
            ram_bytes: Some(16_000_000_000),
            gpu_name: None,
            vram_bytes: Some(8_000_000_000),
            disk_free_bytes: Some(100_000_000_000),
            models: None,
            runtimes: None,
            now_unix: Some(100),
        };
        assert!(!ResourceInventory::from_probes(base.clone()).has_unknown_readiness());

        let missing_vram = ProbeInputs {
            vram_bytes: None,
            ..base
        };
        let inv = ResourceInventory::from_probes(missing_vram);
        assert!(inv.has_unknown_readiness());
        assert!(inv.vram_bytes.is_unknown());
    }

    #[test]
    fn default_probe_inputs_produce_unknown_observations_with_unavailable_provenance() {
        let inv = ResourceInventory::from_probes(ProbeInputs {
            now_unix: Some(42),
            ..ProbeInputs::default()
        });
        assert_eq!(inv.captured_unix, 42);
        assert!(inv.has_unknown_readiness());
        assert_eq!(
            inv.cpu_cores,
            Observation::Unknown {
                observed_unix: 42,
                provenance: ProbeProvenance::Unavailable {
                    reason: "cpu count unavailable".to_string(),
                },
            }
        );
    }

    #[test]
    fn resource_inventory_serde_json_roundtrip_preserves_observations_and_provenance() {
        let inv = ResourceInventory::from_probes(ProbeInputs {
            cpu_cores: Some(12),
            ram_bytes: Some(64_000_000_000),
            gpu_name: None,
            vram_bytes: None,
            disk_free_bytes: Some(250_000_000_000),
            models: Some(vec!["phi4".to_string()]),
            runtimes: None,
            now_unix: Some(777),
        });
        let json = serde_json::to_string(&inv).expect("serialize ResourceInventory");
        let back: ResourceInventory =
            serde_json::from_str(&json).expect("deserialize ResourceInventory");
        assert_eq!(back, inv);
        assert!(back.has_unknown_readiness());
    }
}
