//! Inspect the last mission blackboard (glass-box swarm shared state).
use anyhow::{bail, Result};
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
            let path = workspace.join(".susi").join("last_blackboard.json");
            if !path.is_file() {
                bail!(
                    "no mission blackboard at {} — run a swarm mission first",
                    path.display()
                );
            }
            let text = std::fs::read_to_string(&path)?;
            println!("{}", susi_agents::external::redact(&text));
        }
    }
    Ok(())
}
