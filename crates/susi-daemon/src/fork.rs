//! Memory Forking & Process Cloning (Swarm OS Bullet 83)
//!
//! A cell can request a 'fork' syscall to clone its memory state and parallelize
//! a search tree algorithm across the swarm.

use std::fs;
use std::path::{Path, PathBuf};

/// Manages forking of sandboxed cells.
pub struct ForkManager {
    fork_dir: PathBuf,
}

impl ForkManager {
    /// Initializes the Fork Manager in the workspace.
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        let fork_dir = workspace.join("forks");
        if !fork_dir.exists() {
            fs::create_dir_all(&fork_dir)?;
        }
        Ok(Self { fork_dir })
    }

    /// Forks a running cell by cloning its suspended memory state to a new ID.
    pub fn fork_cell(&self, original_cell_id: &str, current_memory_snapshot: &[u8]) -> std::io::Result<String> {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_micros();
        let new_id = format!("{}_fork_{:x}", original_cell_id, ts);
        
        let target_file = self.fork_dir.join(format!("{}.suspend", new_id));
        
        // Write the cloned state. The Swarm OS scheduler can then pick this up
        // as a new cell to be resumed, effectively parallelizing the execution.
        fs::write(&target_file, current_memory_snapshot)?;
        
        Ok(new_id)
    }

    /// Retrieves a cloned fork's memory and removes it from the queue.
    pub fn claim_fork(&self, fork_id: &str) -> std::io::Result<Vec<u8>> {
        let target_file = self.fork_dir.join(format!("{}.suspend", fork_id));
        
        if !target_file.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Fork state not found for {}", fork_id)
            ));
        }

        let snapshot = fs::read(&target_file)?;
        let _ = fs::remove_file(&target_file);

        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_cell_forking() {
        let temp_dir = env::temp_dir().join(format!("susi_fork_test_{}", std::process::id()));
        let manager = ForkManager::new(&temp_dir).unwrap();

        let parent_memory = b"parent_state_tree_search".to_vec();
        
        // Parent requests a fork
        let child_id = manager.fork_cell("parent-cell-1", &parent_memory).unwrap();
        
        assert!(child_id.starts_with("parent-cell-1_fork_"));

        // Scheduler picks up the fork
        let claimed_memory = manager.claim_fork(&child_id).unwrap();
        assert_eq!(claimed_memory, parent_memory);

        let _ = fs::remove_dir_all(temp_dir);
    }
}
