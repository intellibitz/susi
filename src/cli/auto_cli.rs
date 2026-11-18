//! Zero-config auto substrate status (packs, MCP, models, peers).
use crate::anyhow::Result;
use crate::cli_json::print_json;
use std::path::Path;
pub use susi_cli_defs::auto::AutoCommands;

pub fn execute(action: Option<AutoCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AutoCommands::Status) {
        AutoCommands::Status => {
            // `susi_daemon::host_views` builds the snapshot; the MCP
            // `auto_status` tool returns the same document.
            let status = susi_daemon::host_views::auto_status(workspace)
                .map_err(crate::anyhow::Error::msg)?;
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
