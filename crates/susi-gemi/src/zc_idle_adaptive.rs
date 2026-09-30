//! Model idle timeout that adapts to memory pressure.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdlePolicy {
    pub unload_after_secs: u64,
    pub keep_warm: bool,
}

/// Derive unload timing from memory pressure in `0.0..=1.0` and whether the
/// model is currently serving traffic.
#[must_use]
pub fn idle_timeout(pressure: f64, currently_hot: bool) -> IdlePolicy {
    let p = pressure.clamp(0.0, 1.0);
    if p >= 0.85 {
        return IdlePolicy {
            unload_after_secs: 30,
            keep_warm: false,
        };
    }
    if p >= 0.6 {
        return IdlePolicy {
            unload_after_secs: 120,
            keep_warm: currently_hot,
        };
    }
    IdlePolicy {
        unload_after_secs: if currently_hot { 1800 } else { 600 },
        keep_warm: currently_hot,
    }
}

#[cfg(test)]
mod zc_idle_adaptive_tests {
    use super::*;

    #[test]
    fn zc_idle_adaptive_unloads_sooner_under_pressure() {
        let calm = idle_timeout(0.2, true);
        let pressured = idle_timeout(0.9, true);
        assert!(pressured.unload_after_secs < calm.unload_after_secs);
        assert!(!pressured.keep_warm);
        assert!(calm.keep_warm);
    }
}
