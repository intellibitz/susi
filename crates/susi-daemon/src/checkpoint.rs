//! Cell Memory State Checkpointing (Swarm OS Bullet 63)
//!
//! Allows agents to checkpoint their internal KV cache and memory state to disk,
//! enabling instantaneous resume across daemon restarts or host migrations.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Represents a serialized snapshot of a cell's execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellCheckpoint {
    pub cell_id: String,
    pub timestamp: u64,
    /// Encoded representation of the WASM heap or KV cache state.
    pub memory_state: Vec<u8>,
    pub step_counter: u64,
}

/// File-name stem for a cell id: only `[A-Za-z0-9_-]` survive, so a
/// hostile id (`../../x`, `/etc/y`) can never add path components.
pub(crate) fn cell_file_stem(cell_id: &str) -> String {
    cell_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Manages saving and loading of cell state checkpoints.
pub struct CheckpointManager {
    checkpoint_dir: PathBuf,
}

impl Default for CheckpointManager {
    fn default() -> Self {
        // Owned by the substrate, not the world-writable temp dir, where
        // another local user could pre-plant checkpoints or symlinks.
        Self::new(crate::susi_paths::SusiDirs::data_dir().join("checkpoints"))
    }
}

impl CheckpointManager {
    pub fn new(dir: PathBuf) -> Self {
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        Self {
            checkpoint_dir: dir,
        }
    }

    /// Computes the file path for a cell's checkpoint.
    fn get_path(&self, cell_id: &str) -> PathBuf {
        self.checkpoint_dir
            .join(format!("{}.susi_checkpoint", cell_file_stem(cell_id)))
    }

    /// Saves a snapshot of the cell's memory to disk.
    pub fn save_checkpoint(&self, checkpoint: &CellCheckpoint) -> std::io::Result<()> {
        let payload =
            serde_json::to_vec(checkpoint).map_err(|e| std::io::Error::other(e.to_string()))?;

        let path = self.get_path(&checkpoint.cell_id);
        fs::write(path, payload)
    }

    /// Attempts to load the latest checkpoint for a cell.
    pub fn load_checkpoint(&self, cell_id: &str) -> std::io::Result<CellCheckpoint> {
        let path = self.get_path(cell_id);
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Checkpoint not found",
            ));
        }

        let payload = fs::read(path)?;

        serde_json::from_slice(&payload).map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Removes a cell's checkpoint.
    pub fn clear_checkpoint(&self, cell_id: &str) -> std::io::Result<()> {
        let path = self.get_path(cell_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_file_stems_cannot_traverse() {
        assert_eq!(cell_file_stem("../../etc/x"), "______etc_x");
        assert_eq!(cell_file_stem("cell-01_a"), "cell-01_a");
    }

    #[test]
    fn test_checkpoint_save_and_load() {
        let dir = std::env::temp_dir().join(format!("susi_test_cp_{}", std::process::id()));
        let manager = CheckpointManager::new(dir.clone());

        let cp = CellCheckpoint {
            cell_id: "resilient-agent".to_string(),
            timestamp: 1234567890,
            memory_state: vec![0xCA, 0xFE, 0xBA, 0xBE],
            step_counter: 42,
        };

        // Save
        assert!(manager.save_checkpoint(&cp).is_ok());

        // Load
        let loaded = manager.load_checkpoint("resilient-agent").unwrap();
        assert_eq!(loaded.cell_id, "resilient-agent");
        assert_eq!(loaded.memory_state, vec![0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(loaded.step_counter, 42);

        // Clear
        assert!(manager.clear_checkpoint("resilient-agent").is_ok());
        assert!(manager.load_checkpoint("resilient-agent").is_err());

        let _ = fs::remove_dir_all(dir);
    }
}
