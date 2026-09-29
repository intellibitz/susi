//! `susi setup`: one end-to-end zero-config ecosystem setup command.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use susi_config::setup_workflow::{ConsentKind, SetupPlan};
use susi_paths::SusiDirs;

#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Detect hardware/engines/keys/agents and print the plan (default)
    Plan,
    /// Apply the plan (optionally grant cloud consent)
    Apply {
        /// Grant cloud API key consent in this apply
        #[arg(long)]
        cloud: bool,
    },
}

pub fn execute(action: Option<SetupCommands>) -> Result<()> {
    let home = SusiDirs::config_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(SusiDirs::config_dir);
    match action.unwrap_or(SetupCommands::Plan) {
        SetupCommands::Plan => {
            let plan = SetupPlan::detect(&home);
            print_json(&serde_json::to_value(&plan)?)?;
        }
        SetupCommands::Apply { cloud } => {
            let mut plan = SetupPlan::detect(&home);
            if cloud {
                plan.grant_consent(ConsentKind::CloudApiKey);
            }
            let summary = plan.apply(&home).map_err(anyhow::Error::msg)?;
            print_json(&serde_json::json!({
                "hardware": plan.hardware,
                "engines": plan.engines,
                "keys_present": plan.keys_present,
                "agents": plan.agents,
                "summary": summary,
            }))?;
        }
    }
    Ok(())
}
