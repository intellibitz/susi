//! Ambient context synchronization CLI.
use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum AmbientCommands {
    /// Scan workspace once, record changes, refresh semantic index.
    Pulse,
    /// Start background ambient indexer for this workspace (process-local).
    Start,
}

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
