//! Watchdog Timer for Misbehaving Cells
//!
//! A strict watchdog timer that monitors every cell. If a cell spins without yielding
//! or blocks on I/O for too long, the watchdog marks it for abrupt termination.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Watchdog manager tracking cell heartbeats. `Clone` shares the same
/// heartbeat map — the supervisor attaches a clone while the swarm host
/// keeps its own handle.
#[derive(Clone)]
pub struct WatchdogManager {
    /// Maps cell ID to their last check-in (ping) time.
    heartbeats: Arc<Mutex<HashMap<String, Instant>>>,
    /// Maximum allowed duration without a check-in before a cell is considered dead.
    timeout: Duration,
}

impl Default for WatchdogManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(5)) // Default 5 second timeout
    }
}

impl WatchdogManager {
    pub fn new(timeout: Duration) -> Self {
        Self {
            heartbeats: Arc::new(Mutex::new(HashMap::new())),
            timeout,
        }
    }

    /// How long a cell may stay silent before it is reported dead.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Registers a new cell with the watchdog.
    pub fn register_cell(&self, cell_id: &str) {
        self.register_cell_at(cell_id, Instant::now());
    }

    /// [`Self::register_cell`] at an explicit instant (the clock is an input
    /// so timing behaviour can be checked exactly, not by sleeping).
    fn register_cell_at(&self, cell_id: &str, now: Instant) {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(cell_id.to_string(), now);
    }

    /// Records a heartbeat (ping) from a cell.
    pub fn ping(&self, cell_id: &str) -> Result<(), String> {
        self.ping_at(cell_id, Instant::now())
    }

    /// [`Self::ping`] at an explicit instant.
    fn ping_at(&self, cell_id: &str, now: Instant) -> Result<(), String> {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last_seen) = map.get_mut(cell_id) {
            *last_seen = now;
            Ok(())
        } else {
            Err(format!(
                "Cell {} is not registered with the watchdog",
                cell_id
            ))
        }
    }

    /// Removes a cell from tracking (e.g. on graceful exit).
    pub fn unregister_cell(&self, cell_id: &str) {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(cell_id);
    }

    /// Checks all cells and returns a list of IDs that have exceeded the timeout.
    pub fn find_dead_cells(&self) -> Vec<String> {
        self.find_dead_cells_at(Instant::now())
    }

    /// [`Self::find_dead_cells`] as of an explicit instant.
    fn find_dead_cells_at(&self, now: Instant) -> Vec<String> {
        let map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        let mut dead = Vec::new();

        for (id, last_seen) in map.iter() {
            if now.saturating_duration_since(*last_seen) > self.timeout {
                dead.push(id.clone());
            }
        }

        dead
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watchdog_timeout() {
        // The clock is an input: wall-clock sleeps overshoot under load
        // (the gate saw both cells dead at 50 ms), so the instants are exact.
        let watchdog = WatchdogManager::new(Duration::from_millis(50));
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);

        watchdog.register_cell_at("good-cell", start);
        watchdog.register_cell_at("bad-cell", start);

        // Both are alive at registration and right up to the timeout.
        assert!(watchdog.find_dead_cells_at(start).is_empty());
        assert!(watchdog.find_dead_cells_at(at(50)).is_empty());

        // The good cell pings at 30 ms; at 60 ms only the silent one is dead.
        watchdog.ping_at("good-cell", at(30)).unwrap();
        let dead = watchdog.find_dead_cells_at(at(60));
        assert_eq!(dead, vec!["bad-cell".to_string()]);

        // Past its own deadline (30 + 50 ms) the good cell is dead too.
        let mut dead = watchdog.find_dead_cells_at(at(81));
        dead.sort();
        assert_eq!(dead, vec!["bad-cell".to_string(), "good-cell".to_string()]);
    }

    #[test]
    fn ping_unregistered_cell_returns_error() {
        let watchdog = WatchdogManager::default();
        let err = watchdog
            .ping("missing-cell")
            .expect_err("unregistered cell must error");
        assert!(err.contains("missing-cell"));
    }

    #[test]
    fn empty_watchdog_reports_no_dead_cells_and_exposes_timeout() {
        let watchdog = WatchdogManager::new(Duration::from_secs(7));
        assert_eq!(watchdog.timeout(), Duration::from_secs(7));
        assert!(watchdog.find_dead_cells().is_empty());
    }

    #[test]
    fn unregister_cell_and_reregister_manage_heartbeats_cleanly() {
        let watchdog = WatchdogManager::new(Duration::from_secs(5));
        watchdog.register_cell("cell-a");
        assert!(watchdog.ping("cell-a").is_ok());

        watchdog.unregister_cell("cell-a");
        assert!(watchdog.ping("cell-a").is_err());

        watchdog.register_cell("cell-a");
        assert!(watchdog.ping("cell-a").is_ok());
    }

    #[test]
    fn default_watchdog_uses_five_second_timeout_and_clone_shares_heartbeats() {
        let watchdog = WatchdogManager::default();
        assert_eq!(watchdog.timeout(), Duration::from_secs(5));
        let cloned = watchdog.clone();
        watchdog.register_cell("c1");
        assert!(cloned.ping("c1").is_ok());
        assert!(cloned.find_dead_cells().is_empty());
    }
}
