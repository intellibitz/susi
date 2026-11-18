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

    #[test]
    fn zc_rediscovery_adaptive_returns_sixty_secs_when_initially_stable() {
        assert_eq!(rediscovery_secs(0, 0).secs, 60);
        assert_eq!(rediscovery_secs(0, 2).secs, 60);
    }

    #[test]
    fn zc_rediscovery_adaptive_returns_three_hundred_secs_for_intermediate_stability() {
        for windows in 3..=9 {
            assert_eq!(rediscovery_secs(0, windows).secs, 300);
        }
    }

    #[test]
    fn zc_rediscovery_adaptive_changes_override_high_stable_window_count() {
        assert_eq!(rediscovery_secs(1, 50).secs, 30);
    }

    #[test]
    fn rediscovery_interval_serde_roundtrip_and_exact_ten_window_threshold() {
        let interval = rediscovery_secs(0, 10);
        assert_eq!(interval.secs, 3600);
        let json = serde_json::to_string(&interval).expect("serialize RediscoveryInterval");
        let decoded: RediscoveryInterval =
            serde_json::from_str(&json).expect("deserialize RediscoveryInterval");
        assert_eq!(decoded, interval);
    }
}
