//! Hierarchical Orchestrator Cell Abstraction (Swarm OS Bullet 59)
//!
//! A hierarchical orchestrator cell manages a pool of worker cells,
//! dynamically scaling them up or down based on load or task requirements.

use std::sync::RwLock;

/// Represents the status of a worker cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Busy,
    Offline,
}

/// A worker cell managed by an orchestrator.
#[derive(Debug, Clone)]
pub struct WorkerCell {
    pub cell_id: String,
    pub status: WorkerStatus,
}

/// An Orchestrator that manages a pool of worker cells.
pub struct Orchestrator {
    pub orchestrator_id: String,
    workers: RwLock<Vec<WorkerCell>>,
}

impl Orchestrator {
    pub fn new(orchestrator_id: &str) -> Self {
        Self {
            orchestrator_id: orchestrator_id.to_string(),
            workers: RwLock::new(Vec::new()),
        }
    }

    /// Spawns a new worker and adds it to the pool.
    pub fn scale_up(&self, worker_id: &str) {
        let mut pool = self.workers.write().unwrap_or_else(|e| e.into_inner());
        pool.push(WorkerCell {
            cell_id: worker_id.to_string(),
            status: WorkerStatus::Idle,
        });
    }

    /// Terminates and removes a specific worker from the pool.
    pub fn scale_down(&self, worker_id: &str) -> Result<(), String> {
        let mut pool = self.workers.write().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = pool.iter().position(|w| w.cell_id == worker_id) {
            pool.remove(pos);
            Ok(())
        } else {
            Err(format!("Worker '{}' not found in orchestrator pool", worker_id))
        }
    }

    /// Assigns a task to the next available idle worker.
    /// Returns the ID of the assigned worker.
    pub fn assign_task(&self) -> Option<String> {
        let mut pool = self.workers.write().unwrap_or_else(|e| e.into_inner());
        for worker in pool.iter_mut() {
            if worker.status == WorkerStatus::Idle {
                worker.status = WorkerStatus::Busy;
                return Some(worker.cell_id.clone());
            }
        }
        None
    }

    /// Marks a worker as having completed its task, returning it to the Idle state.
    pub fn complete_task(&self, worker_id: &str) {
        let mut pool = self.workers.write().unwrap_or_else(|e| e.into_inner());
        for worker in pool.iter_mut() {
            if worker.cell_id == worker_id {
                worker.status = WorkerStatus::Idle;
                break;
            }
        }
    }

    /// Returns a summary of the current pool state.
    pub fn pool_summary(&self) -> (usize, usize) {
        let pool = self.workers.read().unwrap_or_else(|e| e.into_inner());
        let total = pool.len();
        let busy = pool.iter().filter(|w| w.status == WorkerStatus::Busy).count();
        (total, busy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_scaling_and_assignment() {
        let orchestrator = Orchestrator::new("boss-cell");
        
        // Scale up
        orchestrator.scale_up("worker-1");
        orchestrator.scale_up("worker-2");
        
        assert_eq!(orchestrator.pool_summary(), (2, 0));
        
        // Assign tasks
        let assigned1 = orchestrator.assign_task().unwrap();
        assert_eq!(assigned1, "worker-1");
        
        let assigned2 = orchestrator.assign_task().unwrap();
        assert_eq!(assigned2, "worker-2");
        
        // Pool exhausted
        assert!(orchestrator.assign_task().is_none());
        assert_eq!(orchestrator.pool_summary(), (2, 2));
        
        // Complete task
        orchestrator.complete_task("worker-1");
        assert_eq!(orchestrator.pool_summary(), (2, 1));
        
        // Scale down
        assert!(orchestrator.scale_down("worker-2").is_ok());
        assert_eq!(orchestrator.pool_summary(), (1, 0));
    }
}
