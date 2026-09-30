//! Adaptive capability rediscovery interval.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RediscoveryInterval {
    pub secs: u64,
}

/// Frequent while the catalog is changing; rare when stable.
#[must_use]
pub fn rediscovery_secs(changes_last_window: u32, stable_windows: u32) -> RediscoveryInterval {
    if changes_last_window > 0 {
        return RediscoveryInterval { secs: 30 };
    }
    if stable_windows >= 10 {
        return RediscoveryInterval { secs: 3600 };
    }
    if stable_windows >= 3 {
        return RediscoveryInterval { secs: 300 };
    }
    RediscoveryInterval { secs: 60 }
}

#[cfg(test)]
mod zc_rediscovery_adaptive_tests {
    use super::*;

    #[test]
    fn zc_rediscovery_adaptive_frequent_when_changing() {
        assert_eq!(rediscovery_secs(2, 0).secs, 30);
        assert_eq!(rediscovery_secs(0, 12).secs, 3600);
        assert!(rediscovery_secs(0, 12).secs > rediscovery_secs(1, 0).secs);
    }
}
