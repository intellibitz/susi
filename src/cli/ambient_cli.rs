//! Ambient context synchronization CLI.
use crate::anyhow::Result;
use std::path::Path;
pub use susi_cli_defs::ambient::AmbientCommands;

pub fn execute(action: Option<AmbientCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AmbientCommands::Pulse) {
        AmbientCommands::Pulse => {
            let report = susi_daemon::ambient::pulse(workspace);
            println!("{}", serde_json::to_string_pretty(&report.to_json())?);
        }
        AmbientCommands::Start => {
            susi_daemon::ambient::start_ambient_indexer(workspace);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ambient": "started",
                    "workspace": workspace.display().to_string(),
                    "interval_secs": 15,
                }))?
            );
        }
    }
    Ok(())
}
