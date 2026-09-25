//! Logical Inference TTL Enforcement (Swarm OS Bullet 82)
//!
//! To prevent runaway infinite loops, cells carry a strict maximum 'time-to-live' (TTL)
//! measured in logical inference steps or IPC messages.

use std::collections::HashMap;
use std::sync::RwLock;

/// Tracks the Time-To-Live for a specific cell.
pub struct CellTtl {
    remaining_steps: u64,
}

impl CellTtl {
    pub fn new(max_steps: u64) -> Self {
        Self {
            remaining_steps: max_steps,
        }
    }

    /// Decrements the TTL by 1. Returns false if the TTL is exhausted.
    pub fn consume_step(&mut self) -> bool {
        if self.remaining_steps > 0 {
            self.remaining_steps -= 1;
            true
        } else {
            false
        }
    }

    /// Returns the remaining steps.
    pub fn remaining(&self) -> u64 {
        self.remaining_steps
    }
}

/// Global manager for enforcing logical TTLs on cells.
pub struct TtlManager {
    ttls: RwLock<HashMap<String, CellTtl>>,
    default_ttl: u64,
}

impl Default for TtlManager {
    fn default() -> Self {
        Self::new(50) // Default 50 logical steps per cell
    }
}

impl TtlManager {
    pub fn new(default_ttl: u64) -> Self {
        Self {
            ttls: RwLock::new(HashMap::new()),
            default_ttl,
        }
    }

    /// Registers a new cell with a specific TTL (or defaults if None).
    pub fn register_cell(&self, cell_id: &str, custom_ttl: Option<u64>) {
        let max_steps = custom_ttl.unwrap_or(self.default_ttl);
        let mut map = self.ttls.write().unwrap_or_else(|e| e.into_inner());
        map.insert(cell_id.to_string(), CellTtl::new(max_steps));
    }

    /// Consumes a logical step for a cell (e.g. prior to executing an inference loop).
    /// Returns an error if the TTL has been completely exhausted.
    pub fn consume_step(&self, cell_id: &str) -> Result<(), String> {
        let mut map = self.ttls.write().unwrap_or_else(|e| e.into_inner());
        
        if let Some(ttl) = map.get_mut(cell_id) {
            if ttl.consume_step() {
                Ok(())
            } else {
                Err(format!("TTL Exhausted: Cell '{}' has exceeded its maximum allowed logical inference steps.", cell_id))
            }
        } else {
            Err(format!("Cell {} is not registered with the TTL Manager.", cell_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logical_ttl_exhaustion() {
        let manager = TtlManager::new(3); // 3 steps max
        
        manager.register_cell("agent-x", None);
        
        assert!(manager.consume_step("agent-x").is_ok()); // step 1
        assert!(manager.consume_step("agent-x").is_ok()); // step 2
        assert!(manager.consume_step("agent-x").is_ok()); // step 3
        
        // Step 4 should fail
        assert!(manager.consume_step("agent-x").is_err());
    }
}
