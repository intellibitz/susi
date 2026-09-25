//! Host Directory Mounting (Swarm OS Bullet 89)
//!
//! Allows cells to securely mount host directories (e.g. `/home/user/code`)
//! via a strict capability grant in the sandbox.

use crate::security::CapabilityPolicy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Represents a mounted directory mapping for a cell.
#[derive(Debug, Clone)]
pub struct MountPoint {
    pub cell_path: String,  // e.g. "/workspace"
    pub host_path: PathBuf, // e.g. "/home/user/code"
    pub read_only: bool,
}

/// Manages secure file system mounts for sandboxed cells.
pub struct MountManager {
    /// Maps a cell ID to a list of its mounted directories.
    mounts: RwLock<HashMap<String, Vec<MountPoint>>>,
}

impl Default for MountManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MountManager {
    pub fn new() -> Self {
        Self {
            mounts: RwLock::new(HashMap::new()),
        }
    }

    /// Attempts to mount a host directory for a cell.
    /// Strictly verifies the `mount` capability.
    pub fn mount_dir(
        &self,
        policy: &CapabilityPolicy,
        cell_path: &str,
        host_path: &Path,
        read_only: bool,
    ) -> Result<(), String> {
        // Verify Capability
        let mut has_mount = false;
        for grant in policy.grants() {
            if grant.capability == "mount" {
                has_mount = true;
                break;
            }
        }

        if !has_mount {
            return Err(format!(
                "Access Denied: Cell '{}' lacks the 'mount' capability.",
                policy.cell_id()
            ));
        }

        let mut map = self.mounts.write().unwrap_or_else(|e| e.into_inner());
        map.entry(policy.cell_id().to_string())
            .or_default()
            .push(MountPoint {
                cell_path: cell_path.to_string(),
                host_path: host_path.to_path_buf(),
                read_only,
            });

        Ok(())
    }

    /// Resolves a cell-side path (e.g. `/workspace/file.txt`) to a host-side path
    /// (e.g. `/home/user/code/file.txt`) if a valid mount exists.
    pub fn resolve_path(
        &self,
        cell_id: &str,
        requested_cell_path: &str,
    ) -> Result<PathBuf, String> {
        let map = self.mounts.read().unwrap_or_else(|e| e.into_inner());

        if let Some(mounts) = map.get(cell_id) {
            for mount in mounts {
                if let Some(relative) = requested_cell_path.strip_prefix(&mount.cell_path) {
                    let relative = relative.trim_start_matches('/');
                    let resolved = mount.host_path.join(relative);

                    // Basic path traversal prevention (sandbox boundary check)
                    if !resolved.starts_with(&mount.host_path) {
                        return Err("Sandbox Escape Detected: Path traversal blocked.".to_string());
                    }

                    return Ok(resolved);
                }
            }
        }

        Err(format!("No mount found for path '{}'", requested_cell_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::CapabilityGrant;

    #[test]
    fn test_mount_capability_and_resolution() {
        let manager = MountManager::new();

        let standard_policy = CapabilityPolicy::new("cell-standard", vec![]);
        let privileged_policy = CapabilityPolicy::new(
            "cell-priv",
            vec![CapabilityGrant {
                capability: "mount".to_string(),
                scope: None,
                ephemeral: false,
            }],
        );

        // Standard cell should fail
        assert!(
            manager
                .mount_dir(&standard_policy, "/work", Path::new("/home/user"), true)
                .is_err()
        );

        // Privileged cell should succeed
        assert!(
            manager
                .mount_dir(&privileged_policy, "/work", Path::new("/home/user"), false)
                .is_ok()
        );

        // Resolve path
        let resolved = manager.resolve_path("cell-priv", "/work/data.txt").unwrap();
        assert_eq!(resolved, Path::new("/home/user/data.txt"));

        // Unknown path
        assert!(
            manager
                .resolve_path("cell-priv", "/other/data.txt")
                .is_err()
        );
    }
}
