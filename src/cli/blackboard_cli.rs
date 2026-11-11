//! Inspect the last mission blackboard (glass-box swarm shared state).
use crate::anyhow::{anyhow, Result};
use std::path::Path;
pub use susi_cli_defs::blackboard::BlackboardCommands;

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
