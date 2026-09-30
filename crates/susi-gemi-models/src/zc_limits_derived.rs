//! Concurrency and memory limits derived from hardware.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedLimits {
    pub max_concurrent: u32,
    pub max_model_bytes: u64,
}

/// Derive limits from CPU count and free RAM.
#[must_use]
pub fn limits_from_hardware(cpus: u32, free_ram_bytes: u64) -> DerivedLimits {
    let max_concurrent = cpus.clamp(1, 16);
    let max_model_bytes = (free_ram_bytes / 2).max(512 * 1024 * 1024);
    DerivedLimits {
        max_concurrent,
        max_model_bytes,
    }
}

#[cfg(test)]
mod zc_limits_derived_tests {
    use super::*;

    #[test]
    fn zc_limits_derived_from_hardware() {
        let l = limits_from_hardware(8, 16 * 1024 * 1024 * 1024);
        assert_eq!(l.max_concurrent, 8);
        assert!(l.max_model_bytes >= 8 * 1024 * 1024 * 1024);
        assert_eq!(limits_from_hardware(0, 0).max_concurrent, 1);
    }
}
