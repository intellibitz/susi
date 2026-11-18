//! Inspect or post to the mission blackboard (posts mirror onto Susi Native Board).
use crate::anyhow::{anyhow, Result};
use std::path::Path;
pub use susi_cli_defs::blackboard::BlackboardCommands;
use susi_core::native_board::{post_blackboard_local, BlackboardPostRequest};

pub fn execute(action: Option<BlackboardCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(BlackboardCommands::Show) {
        BlackboardCommands::Show => {
            let text = susi_core::agent_types::read_persisted_blackboard(workspace)
                .map_err(|e| anyhow!(e))?;
            println!("{text}");
        }
        BlackboardCommands::Post {
            key,
            value,
            author,
            task,
            kind,
            title,
        } => {
            let parsed_value = serde_json::from_str::<serde_json::Value>(&value)
                .unwrap_or(serde_json::Value::String(value));
            let entry = post_blackboard_local(
                Some(workspace),
                &BlackboardPostRequest {
                    key,
                    value: parsed_value,
                    author,
                    task,
                    mirror_to_board: true,
                    kind,
                    title,
                },
            )
            .map_err(|e| anyhow!(e))?;
            println!("{}", serde_json::to_string_pretty(&entry)?);
        }
    }
    Ok(())
}
