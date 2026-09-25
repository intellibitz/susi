//! Global GPU Resource Scheduler (Swarm OS Bullet 48)
//!
//! Tracks a shared VRAM budget across cells so concurrent inference
//! requests fail fast with a clear error instead of racing the GPU into
//! OOM, and lets a finished cell's allocation be returned to the pool.

use std::collections::HashMap;
use std::sync::RwLock;

pub struct GpuScheduler {
    total_vram: usize,
    available_vram: RwLock<usize>,
    allocations: RwLock<HashMap<String, usize>>,
}

impl Default for GpuScheduler {
    fn default() -> Self {
        Self::new(16_000_000)
    }
}

impl GpuScheduler {
    pub fn new(total_vram: usize) -> Self {
        Self {
            total_vram,
            available_vram: RwLock::new(total_vram),
            allocations: RwLock::new(HashMap::new()),
        }
    }

    /// Reserves `amount` bytes of VRAM for `cell_id`. Fails if the shared
    /// budget can't cover it.
    pub fn request_vram(&self, cell_id: &str, amount: usize) -> Result<(), String> {
        let mut vram = self
            .available_vram
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if *vram < amount {
            return Err(format!("OOM: requested {amount} of {vram} available"));
        }
        *vram -= amount;
        *self
            .allocations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(cell_id.to_string())
            .or_insert(0) += amount;
        Ok(())
    }

    /// Releases everything `cell_id` currently holds back to the pool.
    pub fn release_vram(&self, cell_id: &str) {
        if let Some(amount) = self
            .allocations
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cell_id)
        {
            *self
                .available_vram
                .write()
                .unwrap_or_else(|e| e.into_inner()) += amount;
        }
    }

    pub fn available(&self) -> usize {
        *self
            .available_vram
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn total(&self) -> usize {
        self.total_vram
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_reduces_and_release_restores_availability() {
        let sched = GpuScheduler::new(1000);
        sched.request_vram("cell-a", 400).unwrap();
        assert_eq!(sched.available(), 600);
        sched.release_vram("cell-a");
        assert_eq!(sched.available(), 1000);
    }

    #[test]
    fn over_budget_request_fails_without_mutating_state() {
        let sched = GpuScheduler::new(100);
        assert!(sched.request_vram("cell-a", 50).is_ok());
        assert!(sched.request_vram("cell-b", 100).is_err());
        assert_eq!(sched.available(), 50);
    }
}
