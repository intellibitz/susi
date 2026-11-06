//! Tier S substrate status: Pluggable, Sandbox, Governance, Host-contract, Reflexes.
//! The document is `susi_daemon::substrate_status::report`, shared with the
//! MCP `substrate_status` tool.
use crate::cli_json::print_json;
use anyhow::Result;
use std::path::Path;
use susi_daemon::discovery_pipeline::Catalogs;

pub use susi_cli_defs::substrate::*;

/// What `substrate status` primes (see `susi_daemon::substrate_status::PRIMED`).
pub(crate) const PRIMED: Catalogs = susi_daemon::substrate_status::PRIMED;

pub fn execute(action: Option<SubstrateCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(SubstrateCommands::Status) {
        SubstrateCommands::Status => {
            print_json(&susi_daemon::substrate_status::report(workspace))?;
        }
    }
    Ok(())
}
