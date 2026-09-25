//! Sandbox Memory Quota Enforcement (Swarm OS Bullet 26)
//!
//! Enforces strict maximum memory limits per cell. Any cell that exceeds
//! its predefined memory quota is instantly flagged for termination to protect host stability.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

/// Tracks memory allocations for a single sandboxed cell.
pub struct CellMemoryTracker {
    allocated_bytes: AtomicUsize,
    max_quota_bytes: usize,
}

impl CellMemoryTracker {
    pub fn new(max_quota_bytes: usize) -> Self {
        Self {
            allocated_bytes: AtomicUsize::new(0),
            max_quota_bytes,
        }
    }

    /// Attempts to allocate memory. Returns an error if the quota is exceeded.
    pub fn try_allocate(&self, bytes: usize) -> Result<(), String> {
        let current = self.allocated_bytes.load(Ordering::Relaxed);
        if current + bytes > self.max_quota_bytes {
            return Err(format!(
                "Memory Quota Exceeded: Attempted to allocate {} bytes, but only {} bytes remain of the {} byte quota.",
                bytes,
                self.max_quota_bytes - current,
                self.max_quota_bytes
            ));
        }
        
        self.allocated_bytes.fetch_add(bytes, Ordering::AcqRel);
        Ok(())
    }

    /// Frees memory.
    pub fn free(&self, bytes: usize) {
        // Prevent underflow
        let current = self.allocated_bytes.load(Ordering::Relaxed);
        let amount_to_sub = std::cmp::min(current, bytes);
        self.allocated_bytes.fetch_sub(amount_to_sub, Ordering::AcqRel);
    }
}

/// Global manager for enforcing memory quotas across all cells.
pub struct MemoryQuotaManager {
    trackers: RwLock<HashMap<String, std::sync::Arc<CellMemoryTracker>>>,
    default_quota: usize,
}

impl Default for MemoryQuotaManager {
    fn default() -> Self {
        Self::new(256 * 1024 * 1024) // Default 256 MiB quota per cell
    }
}

impl MemoryQuotaManager {
    pub fn new(default_quota: usize) -> Self {
        Self {
            trackers: RwLock::new(HashMap::new()),
            default_quota,
        }
    }

    /// Registers a cell with a specific memory quota.
    pub fn register_cell(&self, cell_id: &str, custom_quota: Option<usize>) {
        let quota = custom_quota.unwrap_or(self.default_quota);
        let mut map = self.trackers.write().unwrap_or_else(|e| e.into_inner());
        map.insert(cell_id.to_string(), std::sync::Arc::new(CellMemoryTracker::new(quota)));
    }

    /// Requests memory allocation for a specific cell.
    pub fn allocate(&self, cell_id: &str, bytes: usize) -> Result<(), String> {
        let map = self.trackers.read().unwrap_or_else(|e| e.into_inner());
        if let Some(tracker) = map.get(cell_id) {
            tracker.try_allocate(bytes)
        } else {
            Err(format!("Cell {} is not registered with the Memory Quota Manager", cell_id))
        }
    }

    /// Frees memory for a specific cell.
    pub fn free(&self, cell_id: &str, bytes: usize) {
        let map = self.trackers.read().unwrap_or_else(|e| e.into_inner());
        if let Some(tracker) = map.get(cell_id) {
            tracker.free(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_quota_enforcement() {
        let manager = MemoryQuotaManager::new(1024); // 1 KB quota
        
        manager.register_cell("heavy-cell", None);
        
        // Allocate 500 bytes (success)
        assert!(manager.allocate("heavy-cell", 500).is_ok());
        
        // Allocate 500 more (success)
        assert!(manager.allocate("heavy-cell", 500).is_ok());
        
        // Allocate 25 more (fail, exceeds quota)
        assert!(manager.allocate("heavy-cell", 25).is_err());
        
        // Free 100 bytes
        manager.free("heavy-cell", 100);
        
        // Allocate 50 bytes (success now)
        assert!(manager.allocate("heavy-cell", 50).is_ok());
    }
}
