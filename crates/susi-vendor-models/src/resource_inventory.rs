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
