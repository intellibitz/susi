//! Restart crashed local engines with capped exponential backoff.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineHandle {
    pub id: String,
    pub healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchdogAction {
    Restart { backoff_ms: u64 },
    MarkUnfit,
    None,
}

#[derive(Debug)]
pub struct EngineWatchdog {
    pub max_crashes: u32,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
    crashes: u32,
    unfit: bool,
}

impl EngineWatchdog {
    #[must_use]
    pub fn new(max_crashes: u32, base_backoff_ms: u64, max_backoff_ms: u64) -> Self {
        Self {
            max_crashes: max_crashes.max(1),
            base_backoff_ms,
            max_backoff_ms: max_backoff_ms.max(base_backoff_ms),
            crashes: 0,
            unfit: false,
        }
    }

    pub fn on_health_check(&mut self, engine: &EngineHandle) -> WatchdogAction {
        if self.unfit {
            return WatchdogAction::None;
        }
        if engine.healthy {
            return WatchdogAction::None;
        }
        self.crashes = self.crashes.saturating_add(1);
        if self.crashes >= self.max_crashes {
            self.unfit = true;
            return WatchdogAction::MarkUnfit;
        }
        let shift = (self.crashes - 1).min(16);
        let backoff = self
            .base_backoff_ms
            .saturating_mul(1u64 << shift)
            .min(self.max_backoff_ms);
        WatchdogAction::Restart {
            backoff_ms: backoff,
        }
    }

    #[must_use]
    pub fn is_unfit(&self) -> bool {
        self.unfit
    }
}

#[cfg(test)]
mod engine_watchdog_tests {
    use super::*;

    #[test]
    fn engine_watchdog_restarts_with_backoff_then_marks_unfit() {
        let mut w = EngineWatchdog::new(3, 100, 1000);
        let down = EngineHandle {
            id: "llama".into(),
            healthy: false,
        };
        assert_eq!(
            w.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 100 }
        );
        assert_eq!(
            w.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 200 }
        );
        assert_eq!(w.on_health_check(&down), WatchdogAction::MarkUnfit);
        assert!(w.is_unfit());
        assert_eq!(w.on_health_check(&down), WatchdogAction::None);
    }
}
