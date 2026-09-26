//! Cell Hibernation & Suspension (Swarm OS Bullet 11)
//!
//! Exposes the kernel capabilities to suspend a running cell (writing its memory
//! and stack to disk to free RAM) and resume it seamlessly when a new message arrives.
//! Particularly implemented for WASM cells where linear memory can be fully serialized.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Represents a suspended cell's state on disk.
#[derive(Debug, Clone)]
pub struct SuspendedCell {
    pub cell_id: String,
    pub state_file: PathBuf,
    pub suspended_at: u64,
}

pub struct HibernationManager {
    storage_dir: PathBuf,
}

impl HibernationManager {
    /// Initializes the Hibernation Manager with the given workspace.
    pub fn new(workspace: &Path) -> std::io::Result<Self> {
        let storage_dir = workspace.join("suspended_cells");
        if !storage_dir.exists() {
            fs::create_dir_all(&storage_dir)?;
        }
        Ok(Self { storage_dir })
    }

    /// Suspends a cell by writing its linear memory / state to disk.
    /// In a full implementation for WASM, this serializes the Wasmtime Store.
    pub fn suspend_cell(
        &self,
        cell_id: &str,
        memory_snapshot: &[u8],
    ) -> std::io::Result<SuspendedCell> {
        let state_file = self.storage_dir.join(format!(
            "{}.suspend",
            crate::checkpoint::cell_file_stem(cell_id)
        ));

        // Write the snapshot to disk to free RAM
        fs::write(&state_file, memory_snapshot)?;

        let suspended_at = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(SuspendedCell {
            cell_id: cell_id.to_string(),
            state_file,
            suspended_at,
        })
    }

    /// Resumes a cell by reading its state back into memory.
    pub fn resume_cell(&self, cell_id: &str) -> std::io::Result<Vec<u8>> {
        let state_file = self.storage_dir.join(format!(
            "{}.suspend",
            crate::checkpoint::cell_file_stem(cell_id)
        ));

        if !state_file.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Suspended state not found for cell {}", cell_id),
            ));
        }

        let snapshot = fs::read(&state_file)?;

        // Remove the state file once rehydrated
        let _ = fs::remove_file(&state_file);

        Ok(snapshot)
    }

    /// Lists all currently suspended cells.
    pub fn list_suspended(&self) -> std::io::Result<Vec<String>> {
        let mut cells = Vec::new();
        if self.storage_dir.exists() {
            for entry in fs::read_dir(&self.storage_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("suspend") {
                    // Allow the inner if let to satisfy the linter while keeping it readable.
                    #[allow(clippy::collapsible_if)]
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        cells.push(stem.to_string());
                    }
                }
            }
        }
        Ok(cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_suspend_and_resume() {
        let temp_dir =
            env::temp_dir().join(format!("susi_hibernation_test_{}", std::process::id()));
        let manager = HibernationManager::new(&temp_dir).unwrap();

        let dummy_memory = vec![0xDE, 0xAD, 0xBE, 0xEF];

        // Suspend the cell
        let suspended = manager.suspend_cell("test-cell-1", &dummy_memory).unwrap();
        assert!(suspended.state_file.exists());

        // List suspended
        let suspended_list = manager.list_suspended().unwrap();
        assert_eq!(suspended_list.len(), 1);
        assert_eq!(suspended_list[0], "test-cell-1");

        // Resume the cell
        let resumed_memory = manager.resume_cell("test-cell-1").unwrap();
        assert_eq!(resumed_memory, dummy_memory);

        // State file should be deleted after resume
        assert!(!suspended.state_file.exists());

        let _ = fs::remove_dir_all(temp_dir);
    }
}
