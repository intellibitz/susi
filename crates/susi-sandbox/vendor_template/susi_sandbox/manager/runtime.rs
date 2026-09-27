//! Sandbox runtime helpers: docker exec, audit, backup, intent bundles, memory.
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::susi_config::confined_workspace_join;
use crate::susi_config::SusiConfig;
use crate::susi_config::*;

// === SANDBOX MANAGER ===
pub struct SandboxManager;

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        // bollard lives only in the standalone `susi-sandbox` service binary;
        // consumers reach it over IPC. No local fallback — Docker isolation
        // must not be re-implemented without bollard in every feature crate.
        match crate::susi_sandbox::service::docker_exec(cmd) {
            Some(out) => Ok(out),
            None => Err(EaiError::process(
                "susi-sandbox service unreachable; docker exec requires the sandbox service on 127.0.0.1:18083 (SUSI_SANDBOX_PORT)",
            )),
        }
    }

    pub fn ensure_global_sandbox(global_dir: &Path) -> EaiResult<()> {
        if crate::susi_sandbox::service::ensure_global(global_dir) {
            return Ok(());
        }
        Self::ensure_global_sandbox_locally(global_dir)
    }
}

#[path = "../../../src/manager/runtime_shared.rs"]
mod shared;
pub use shared::*;
