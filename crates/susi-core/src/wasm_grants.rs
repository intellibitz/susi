//! Enforce reflex grant derivation and containment (VC-201-075).
//!
//! Per-reflex grants derived from the parent mission — filesystem, network,
//! memory, fuel, and time — such that generated code cannot acquire permissions
//! or widen its privileges beyond what the parent holds.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Resource limits bounding reflex execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_fuel: u64,
    pub timeout_millis: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 16 * 1024 * 1024, // 16 MiB
            max_fuel: 100_000,
            timeout_millis: 5_000,
        }
    }
}

impl ResourceLimits {
    /// Constrain child limits such that child <= parent on all dimensions.
    #[must_use]
    pub fn clamp_to_parent(&self, parent: &ResourceLimits) -> Self {
        Self {
            max_memory_bytes: self.max_memory_bytes.min(parent.max_memory_bytes),
            max_fuel: self.max_fuel.min(parent.max_fuel),
            timeout_millis: self.timeout_millis.min(parent.timeout_millis),
        }
    }
}

/// An immutable, non-widening set of grants and resource bounds for an execution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSet {
    caps: BTreeSet<String>,
    limits: ResourceLimits,
}

impl GrantSet {
    /// Construct a grant set from explicit capabilities and resource limits.
    #[must_use]
    pub fn from_caps_and_limits(caps: BTreeSet<String>, limits: ResourceLimits) -> Self {
        Self { caps, limits }
    }

    /// Baseline capabilities and resource limits for the host environment.
    #[must_use]
    pub fn host_baseline() -> Self {
        Self {
            caps: ["fs.read", "net.loopback", "clock"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            limits: ResourceLimits {
                max_memory_bytes: 256 * 1024 * 1024,
                max_fuel: 1_000_000,
                timeout_millis: 30_000,
            },
        }
    }

    /// Read-only view of the granted capabilities.
    #[must_use]
    pub fn caps(&self) -> &BTreeSet<String> {
        &self.caps
    }

    /// Read-only view of the resource limits.
    #[must_use]
    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// Check if a capability is allowed by this grant set.
    #[must_use]
    pub fn allows(&self, cap: &str) -> bool {
        self.caps.contains(cap)
    }

    /// Check if all required capabilities are allowed.
    #[must_use]
    pub fn allows_all(&self, required: &BTreeSet<String>) -> bool {
        required.is_subset(&self.caps)
    }

    /// Check if resource usage is within limits.
    #[must_use]
    pub fn allows_resources(&self, memory_bytes: u64, fuel: u64, timeout_ms: u64) -> bool {
        memory_bytes <= self.limits.max_memory_bytes
            && fuel <= self.limits.max_fuel
            && timeout_ms <= self.limits.timeout_millis
    }

    /// Derive child reflex grants from this parent mission grant set.
    ///
    /// The child can NEVER acquire permissions the parent lacks (intersection),
    /// and resource limits are clamped so the child cannot exceed the parent.
    #[must_use]
    pub fn derive_for_reflex(
        &self,
        requested_caps: &BTreeSet<String>,
        requested_limits: ResourceLimits,
    ) -> Self {
        let allowed_caps: BTreeSet<String> =
            requested_caps.intersection(&self.caps).cloned().collect();
        let clamped_limits = requested_limits.clamp_to_parent(&self.limits);
        Self {
            caps: allowed_caps,
            limits: clamped_limits,
        }
    }

    /// Generated Wasm receives an attenuated subset strictly bounded by the parent.
    #[must_use]
    pub fn attenuate_for_wasm(&self) -> Self {
        let wasm_allow = BTreeSet::from(["fs.read".to_string(), "clock".to_string()]);
        self.derive_for_reflex(&wasm_allow, ResourceLimits::default())
    }
}
