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

    #[test]
    fn engine_watchdog_healthy_check_returns_none_and_stays_fit() {
        let mut w = EngineWatchdog::new(2, 50, 500);
        let ok = EngineHandle {
            id: "llama".into(),
            healthy: true,
        };
        assert_eq!(w.on_health_check(&ok), WatchdogAction::None);
        assert!(!w.is_unfit());
    }

    #[test]
    fn engine_watchdog_exponential_backoff_caps_at_max_backoff_ms() {
        let mut w = EngineWatchdog::new(5, 300, 500);
        let down = EngineHandle {
            id: "qwen".into(),
            healthy: false,
        };
        assert_eq!(
            w.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 300 }
        );
        assert_eq!(
            w.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 500 }
        );
        assert_eq!(
            w.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 500 }
        );
        assert!(!w.is_unfit());
    }

    #[test]
    fn engine_watchdog_healthy_interlude_preserves_crash_counter_until_unfit() {
        let w = EngineWatchdog::new(0, 200, 100);
        assert_eq!(w.max_crashes, 1);
        assert_eq!(w.max_backoff_ms, 200);

        let mut w2 = EngineWatchdog::new(3, 100, 1000);
        let down = EngineHandle {
            id: "llama".into(),
            healthy: false,
        };
        let up = EngineHandle {
            id: "llama".into(),
            healthy: true,
        };
        assert_eq!(
            w2.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 100 }
        );
        assert_eq!(w2.on_health_check(&up), WatchdogAction::None);
        assert_eq!(
            w2.on_health_check(&down),
            WatchdogAction::Restart { backoff_ms: 200 }
        );
        assert_eq!(w2.on_health_check(&down), WatchdogAction::MarkUnfit);
        assert_eq!(w2.on_health_check(&up), WatchdogAction::None);
    }

    #[test]
    fn engine_handle_and_watchdog_action_serde_roundtrip() {
        let handle = EngineHandle {
            id: "ollama-local".into(),
            healthy: true,
        };
        let h_json = serde_json::to_string(&handle).expect("serialize handle");
        let h_back: EngineHandle = serde_json::from_str(&h_json).expect("deserialize handle");
        assert_eq!(h_back, handle);

        for action in [
            WatchdogAction::Restart { backoff_ms: 250 },
            WatchdogAction::MarkUnfit,
            WatchdogAction::None,
        ] {
            let a_json = serde_json::to_string(&action).expect("serialize action");
            let a_back: WatchdogAction = serde_json::from_str(&a_json).expect("deserialize action");
            assert_eq!(a_back, action);
        }
    }
}
