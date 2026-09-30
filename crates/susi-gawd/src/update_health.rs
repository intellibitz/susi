//! Release-sync health telemetry and rollback.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncHealth {
    Ok,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub health: SyncHealth,
    pub rollback: bool,
}

/// Decide whether to keep the new binary or roll back after a health probe.
#[must_use]
pub fn evaluate_sync(health_ok: bool, version_matches: bool) -> SyncReport {
    if health_ok && version_matches {
        SyncReport {
            health: SyncHealth::Ok,
            rollback: false,
        }
    } else if !health_ok {
        SyncReport {
            health: SyncHealth::Failed,
            rollback: true,
        }
    } else {
        SyncReport {
            health: SyncHealth::Degraded,
            rollback: true,
        }
    }
}

#[cfg(test)]
mod update_health_tests {
    use super::*;

    #[test]
    fn update_health_rolls_back_on_probe_failure() {
        assert!(!evaluate_sync(true, true).rollback);
        assert!(evaluate_sync(false, true).rollback);
        assert_eq!(evaluate_sync(false, true).health, SyncHealth::Failed);
    }
}
