//! IPC-first wrapper over the canonical local daemon-state implementation:
//! each call asks the `susi-sandbox` service first and falls back to the
//! local filesystem helpers when it is unreachable.

use std::path::Path;

#[path = "../../src/daemon_state.rs"]
mod local;

pub struct SusiDaemonState;

impl SusiDaemonState {
    pub fn calculate_binary_hash(bin_path: &Path) -> std::io::Result<String> {
        local::SusiDaemonState::calculate_binary_hash(bin_path)
    }

    pub fn calculate_binary_hash_cached(
        bin_path: &Path,
        global_dir: &Path,
    ) -> std::io::Result<String> {
        if let Some(hash) = super::service::hash_cached(bin_path, global_dir) {
            return Ok(hash);
        }
        local::SusiDaemonState::calculate_binary_hash_cached(bin_path, global_dir)
    }

    pub fn verify_binary_integrity(bin_path: &Path, global_dir: &Path) -> std::io::Result<bool> {
        if let Some(ok) = super::service::verify_integrity(bin_path, global_dir) {
            return Ok(ok);
        }
        local::SusiDaemonState::verify_binary_integrity(bin_path, global_dir)
    }

    pub fn check_status(workspace: &Path, global_dir: &Path) -> bool {
        if let Some(status) = super::service::check_status(workspace, global_dir) {
            return status;
        }
        local::SusiDaemonState::check_status(workspace, global_dir)
    }
}
