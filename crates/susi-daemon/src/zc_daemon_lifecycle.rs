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
}
