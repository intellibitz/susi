//! Watchdog Timer for Misbehaving Cells (Swarm OS Bullet 55)
//!
//! A strict watchdog timer that monitors every cell. If a cell spins without yielding
//! or blocks on I/O for too long, the watchdog marks it for abrupt termination.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Watchdog manager tracking cell heartbeats.
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

    /// Registers a new cell with the watchdog.
    pub fn register_cell(&self, cell_id: &str) {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(cell_id.to_string(), Instant::now());
    }

    /// Records a heartbeat (ping) from a cell.
    pub fn ping(&self, cell_id: &str) -> Result<(), String> {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last_seen) = map.get_mut(cell_id) {
            *last_seen = Instant::now();
            Ok(())
        } else {
            Err(format!("Cell {} is not registered with the watchdog", cell_id))
        }
    }

    /// Removes a cell from tracking (e.g. on graceful exit).
    pub fn unregister_cell(&self, cell_id: &str) {
        let mut map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(cell_id);
    }

    /// Checks all cells and returns a list of IDs that have exceeded the timeout.
    pub fn find_dead_cells(&self) -> Vec<String> {
        let map = self.heartbeats.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut dead = Vec::new();
        
        for (id, last_seen) in map.iter() {
            if now.duration_since(*last_seen) > self.timeout {
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
        let watchdog = WatchdogManager::new(Duration::from_millis(50));
        
        watchdog.register_cell("good-cell");
        watchdog.register_cell("bad-cell");
        
        // Both should be alive initially
        assert_eq!(watchdog.find_dead_cells().len(), 0);
        
        // Sleep to let them approach timeout
        std::thread::sleep(Duration::from_millis(30));
        
        // Good cell pings
        watchdog.ping("good-cell").unwrap();
        
        // Sleep past the timeout threshold
        std::thread::sleep(Duration::from_millis(30));
        
        // Find dead cells
        let dead = watchdog.find_dead_cells();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0], "bad-cell");
        
        // Good cell should still be alive because it pinged
        assert!(!dead.contains(&"good-cell".to_string()));
    }
}
