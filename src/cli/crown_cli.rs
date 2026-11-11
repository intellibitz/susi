//! Tier S crown: verify every USP holds in source/runtime (not marketing).
//!
//! Critical checks must pass for exit 0. Host-gated capabilities report
//! `ready`/`optional` without failing the crown when the host lacks Docker/daemon.
use crate::anyhow::{bail, Result};
use crate::cli_json::print_json;
use std::path::Path;
pub use susi_cli_defs::crown::CrownCommands;
use susi_daemon::discovery_pipeline::Catalogs;

/// What `crown verify|status` primes (see `susi_daemon::crown::PRIMED`).
pub(crate) const PRIMED: Catalogs = susi_daemon::crown::PRIMED;

pub fn execute(action: Option<CrownCommands>, workspace: &Path) -> Result<()> {
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);

    match action.unwrap_or(CrownCommands::Verify) {
        CrownCommands::Status => {
            print_json(&susi_daemon::crown::report(workspace))?;
            Ok(())
        }
        CrownCommands::Verify => {
            let body = susi_daemon::crown::report(workspace);
            print_json(&body)?;
            if body.get("all_critical_hold").and_then(|v| v.as_bool()) != Some(true) {
                bail!("one or more critical Tier S USPs failed crown verify");
            }
            Ok(())
        }
    }
}
