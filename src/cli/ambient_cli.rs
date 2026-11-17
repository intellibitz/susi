//! Ambient context synchronization CLI.
use crate::anyhow::Result;
use std::path::Path;
pub use susi_cli_defs::ambient::AmbientCommands;

pub fn execute(action: Option<AmbientCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AmbientCommands::Pulse) {
        AmbientCommands::Pulse => {
            let report = susi_daemon::ambient::pulse(workspace);
            if let Some(error) = report.get("error").and_then(|v| v.as_str()) {
                return Err(crate::anyhow::Error::msg(error.to_string()));
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        AmbientCommands::Start => {
            let report = susi_daemon::ambient::start_report(workspace);
            if let Some(error) = report.get("error").and_then(|v| v.as_str()) {
                return Err(crate::anyhow::Error::msg(error.to_string()));
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}
