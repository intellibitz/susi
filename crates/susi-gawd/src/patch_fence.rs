//! Fence autonomous patches into isolated workspaces (VC-201-013).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchFence {
    pub patch_id: String,
    pub workspace: PathBuf,
    pub applied: bool,
}

impl PatchFence {
    #[must_use]
    pub fn isolate(root: &Path, patch_id: &str) -> Self {
        let workspace = root.join("patches").join(patch_id);
        Self {
            patch_id: patch_id.to_string(),
            workspace,
            applied: false,
        }
    }

    pub fn ensure_isolated(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.workspace)
    }

    pub fn apply_in_isolation(&mut self) -> Result<(), String> {
        if !self.workspace.exists() {
            return Err("workspace missing".into());
        }
        // Never apply outside the fenced path.
        self.applied = true;
        Ok(())
    }

    #[must_use]
    pub fn is_inside_fence(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace)
    }
}
