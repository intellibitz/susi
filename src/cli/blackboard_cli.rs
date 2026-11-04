//! Inspect the last mission blackboard (glass-box swarm shared state).
use anyhow::{anyhow, Result};
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum BlackboardCommands {
    /// Show `.susi/last_blackboard.json` from the current workspace (default)
    Show,
}

pub fn execute(action: Option<BlackboardCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(BlackboardCommands::Show) {
        BlackboardCommands::Show => {
            let text = susi_core::agent_types::read_persisted_blackboard(workspace)
                .map_err(|e| anyhow!(e))?;
            println!("{text}");
        }
    }
    Ok(())
}
