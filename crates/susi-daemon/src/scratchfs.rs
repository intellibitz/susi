//! Ephemeral ScratchFS for Sandboxed Cells (Swarm OS Bullet 32)
//!
//! Provides a managed ephemeral in-memory (or highly temporary) filesystem
//! that is mounted into each sandbox for scratch space. Cleanly wiped
//! when the cell terminates.

use std::fs;
use std::path::PathBuf;

/// A managed scratch directory for a single cell.
pub struct ScratchFs {
    cell_id: String,
    path: PathBuf,
}

impl ScratchFs {
    /// Mounts a new scratch space for the given cell.
    pub fn mount(cell_id: &str) -> std::io::Result<Self> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // The cell id only names the dir: keep it to a safe charset so it
        // can never add path components (`../`).
        let safe_id: String = cell_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = std::env::temp_dir().join(format!("susi_scratch_{safe_id}_{ts}"));

        // Exclusive create, owner-only: in the shared temp dir an existing
        // entry (e.g. a planted symlink) must fail rather than be reused.
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;

        Ok(Self {
            cell_id: cell_id.to_string(),
            path,
        })
    }

    /// Returns the absolute path to the scratch space.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Returns the cell ID this scratch space belongs to.
    pub fn cell_id(&self) -> &str {
        &self.cell_id
    }

    /// Explicitly wipes the scratch space.
    pub fn wipe(&self) -> std::io::Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }
}

/// Automatically wipes the scratch space when the cell terminates and this is dropped.
impl Drop for ScratchFs {
    fn drop(&mut self) {
        let _ = self.wipe();
    }
}

/// Global ScratchFS Manager.
pub struct ScratchFsManager {
    active_mounts: std::sync::RwLock<std::collections::HashMap<String, PathBuf>>,
}

impl ScratchFsManager {
    pub fn new() -> Self {
        Self {
            active_mounts: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Creates and registers a scratch space for a cell.
    pub fn allocate(&self, cell_id: &str) -> std::io::Result<ScratchFs> {
        let fs = ScratchFs::mount(cell_id)?;
        self.active_mounts
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cell_id.to_string(), fs.path().to_path_buf());
        Ok(fs)
    }

    /// Removes a cell's registration. The actual wipe is handled by `ScratchFs::drop`.
    pub fn release(&self, cell_id: &str) {
        self.active_mounts
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cell_id);
    }
}

impl Default for ScratchFsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_ids_cannot_escape_the_temp_dir() {
        let fs = ScratchFs::mount("../../evil").unwrap();
        assert_eq!(fs.path().parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs.wipe();
    }

    #[test]
    fn test_scratchfs_lifecycle() {
        let manager = ScratchFsManager::new();

        let path = {
            let fs = manager.allocate("test-cell-99").unwrap();
            let p = fs.path().to_path_buf();
            assert!(p.exists());

            // Write some test data
            fs::write(p.join("test.txt"), "hello").unwrap();

            p // Return the path out of this scope to test drop behavior
        }; // fs is dropped here, which should trigger `wipe()`

        // Verify it was wiped
        assert!(!path.exists());
    }
}
