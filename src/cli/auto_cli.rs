//! Zero-config auto substrate status (packs, MCP, models, peers).
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum AutoCommands {
    /// Show zero-config auto substrate readiness (default). A snapshot of
    /// what the last prime wrote: the full report is refreshed only by
    /// `auto prime` and commands that prime every catalog (missions,
    /// in-process `mcp call`); the daemon and other commands prime only
    /// what they read, and the daemon never probes agent frameworks.
    Status,
    /// Re-run auto-prime now (models/installed peers; never enables MCP)
    Prime,
}

pub fn execute(action: Option<AutoCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AutoCommands::Status) {
        AutoCommands::Status => {
            // `susi_daemon::host_views` builds the snapshot; the MCP
            // `auto_status` tool returns the same document.
            let status =
                susi_daemon::host_views::auto_status(workspace).map_err(anyhow::Error::msg)?;
            print_json(&status)?;
            if status.local_models_installed.is_empty() {
                eprintln!("local model: {}", status.local_model);
            }
            if status.last_auto_prime.is_none() {
                eprintln!(
                    "hint: no full prime recorded yet; `susi auto prime` refreshes this snapshot"
                );
            }
        }
        AutoCommands::Prime => {
            print_json(&susi_daemon::host_views::auto_prime())?;
        }
    }
    Ok(())
}
