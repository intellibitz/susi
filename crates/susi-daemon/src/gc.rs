//! Swarm Garbage Collector (Swarm OS Bullet 20)
//!
//! A background 'Garbage Collector' daemon that periodically cleans up terminated cells,
//! reclaiming memory, network ports, and scratch disk space.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Represents a tracked resource allocated to a cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CellResource {
    MemoryBytes(usize),
    NetworkPort(u16),
    ScratchDir(String),
}

/// The Garbage Collector tracks allocated resources and cleans them up upon cell termination.
pub struct GarbageCollector {
    /// Maps a cell ID to the set of resources it has allocated.
    allocations: RwLock<HashMap<String, HashSet<CellResource>>>,
}

impl Default for GarbageCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl GarbageCollector {
    pub fn new() -> Self {
        Self {
            allocations: RwLock::new(HashMap::new()),
        }
    }

    /// Records an allocation made by a cell.
    pub fn track_allocation(&self, cell_id: &str, resource: CellResource) {
        let mut map = self.allocations.write().unwrap_or_else(|e| e.into_inner());
        map.entry(cell_id.to_string())
           .or_default()
           .insert(resource);
    }

    /// Executes the garbage collection sweep for a terminated cell, returning
    /// the list of reclaimed resources.
    pub fn sweep_cell(&self, cell_id: &str) -> Vec<CellResource> {
        let mut map = self.allocations.write().unwrap_or_else(|e| e.into_inner());
        if let Some(resources) = map.remove(cell_id) {
            // In a real system, we would actively free the memory, close ports, and delete dirs here.
            resources.into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Returns the number of currently tracked cells.
    pub fn tracked_cells(&self) -> usize {
        let map = self.allocations.read().unwrap_or_else(|e| e.into_inner());
        map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_garbage_collector_sweep() {
        let gc = GarbageCollector::new();
        
        // Track allocations
        gc.track_allocation("agent-1", CellResource::NetworkPort(8080));
        gc.track_allocation("agent-1", CellResource::MemoryBytes(1024));
        gc.track_allocation("agent-2", CellResource::NetworkPort(8081));
        
        assert_eq!(gc.tracked_cells(), 2);
        
        // Sweep agent-1
        let reclaimed = gc.sweep_cell("agent-1");
        assert_eq!(reclaimed.len(), 2);
        assert!(reclaimed.contains(&CellResource::NetworkPort(8080)));
        assert!(reclaimed.contains(&CellResource::MemoryBytes(1024)));
        
        assert_eq!(gc.tracked_cells(), 1);
        
        // Sweep again yields nothing
        assert_eq!(gc.sweep_cell("agent-1").len(), 0);
    }
}
