//! Daemon starts on first use; restarts when binary mtime changes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonLifecycle {
    pub running: bool,
    pub binary_mtime: u64,
}

impl DaemonLifecycle {
    #[must_use]
    pub fn ensure_started(mut self, want_mtime: u64) -> Self {
        if !self.running {
            self.running = true;
            self.binary_mtime = want_mtime;
            return self;
        }
        if want_mtime > self.binary_mtime {
            // restart on binary change
            self.binary_mtime = want_mtime;
        }
        self
    }

    #[must_use]
    pub fn needs_restart(&self, binary_mtime: u64) -> bool {
        self.running && binary_mtime > self.binary_mtime
    }
}

#[cfg(test)]
mod zc_daemon_lifecycle_tests {
    use super::*;

    #[test]
    fn zc_daemon_lifecycle_starts_and_restarts_on_binary_change() {
        let d = DaemonLifecycle {
            running: false,
            binary_mtime: 0,
        };
        let d = d.ensure_started(10);
        assert!(d.running);
        assert!(d.needs_restart(20));
        let d2 = d.ensure_started(20);
        assert!(!d2.needs_restart(20));
    }

    #[test]
    fn stopped_lifecycle_never_needs_restart() {
        let d = DaemonLifecycle {
            running: false,
            binary_mtime: 10,
        };
        assert!(!d.needs_restart(20));
        assert!(!d.needs_restart(0));
    }

    #[test]
    fn ensure_started_with_same_or_older_mtime_preserves_existing_mtime() {
        let d = DaemonLifecycle {
            running: true,
            binary_mtime: 50,
        };
        let same = d.clone().ensure_started(50);
        assert!(same.running);
        assert_eq!(same.binary_mtime, 50);

        let older = d.ensure_started(30);
        assert!(older.running);
        assert_eq!(older.binary_mtime, 50);
    }

    #[test]
    fn daemon_lifecycle_serde_json_roundtrip() {
        let d = DaemonLifecycle {
            running: true,
            binary_mtime: 1_700_000_000,
        };
        let json = serde_json::to_string(&d).expect("serialize DaemonLifecycle");
        let restored: DaemonLifecycle =
            serde_json::from_str(&json).expect("deserialize DaemonLifecycle");
        assert_eq!(restored, d);
    }

    #[test]
    fn ensure_started_advances_across_multiple_sequential_binary_upgrades() {
        let mut d = DaemonLifecycle {
            running: false,
            binary_mtime: 0,
        };
        for mtime in [100_u64, 200, 350] {
            d = d.ensure_started(mtime);
            assert!(d.running);
            assert_eq!(d.binary_mtime, mtime);
            assert!(!d.needs_restart(mtime));
        }
    }
}
