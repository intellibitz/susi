//! Model-store disk budget as a safe share of free space.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskBudget {
    pub max_bytes: u64,
    pub share: f64,
}

/// Default share of free disk reserved for model storage.
const DEFAULT_SHARE: f64 = 0.25;

/// Cap store budget at `share` of free bytes, never below a small floor.
#[must_use]
pub fn disk_budget(free_bytes: u64) -> DiskBudget {
    let share = DEFAULT_SHARE;
    let max = ((free_bytes as f64) * share) as u64;
    DiskBudget {
        max_bytes: max.max(256 * 1024 * 1024),
        share,
    }
}

#[cfg(test)]
mod zc_disk_default_tests {
    use super::*;

    #[test]
    fn zc_disk_default_scales_with_free_space() {
        let small = disk_budget(4 * 1024 * 1024 * 1024);
        let large = disk_budget(100 * 1024 * 1024 * 1024);
        assert!(large.max_bytes > small.max_bytes);
        assert!((small.share - 0.25).abs() < f64::EPSILON);
        assert_eq!(disk_budget(100).max_bytes, 256 * 1024 * 1024);
    }
}
