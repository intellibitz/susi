//! Deterministic top frontier model control plane (no daemon for list/doctor/setup).
use crate::cli_json::print_json;
use anyhow::{bail, Error, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum FrontierCommands {
    /// Show the top frontier models and readiness
    List,
    /// Check credentials/endpoints (does not call a paid model)
    Doctor { model: Option<String> },
    /// Show setup instructions and effective wiring
    Setup { model: String },
    /// Override engine/model/protocol/api_key_env via JSON file
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled entry
    Reset { model: String },
    /// Prefer a frontier model for subsequent routing
    Prefer {
        model: Option<String>,
        /// Clear the preferred frontier pin
        #[arg(long)]
        clear: bool,
    },
    /// Probe: short completion via cloud or local endpoint
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
}

pub fn execute(action: Option<FrontierCommands>) -> Result<()> {
    // `susi_daemon::provider_ops` builds every document; the MCP
    // `provider_*` tools return the same ones.
    use susi_daemon::provider_ops::{self as ops, Vendor};
    let action = action.unwrap_or(FrontierCommands::List);
    match action {
        FrontierCommands::List => print_json(&ops::list(Vendor::Frontier).map_err(Error::msg)?)?,
        FrontierCommands::Doctor { model } => {
            let docs =
                ops::doctor(Vendor::Frontier, model.as_deref(), false).map_err(Error::msg)?;
            for doc in &docs {
                print_json(doc)?;
            }
            if !ops::all_ready(&docs) {
                bail!("one or more frontier models need setup");
            }
        }
        FrontierCommands::Setup { model } => {
            print_json(&ops::setup(Vendor::Frontier, Some(&model)).map_err(Error::msg)?)?
        }
        FrontierCommands::Configure { model, file } => {
            let over: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&ops::configure(Vendor::Frontier, &model, over).map_err(Error::msg)?)?;
        }
        FrontierCommands::Reset { model } => {
            print_json(&ops::reset(Vendor::Frontier, &model).map_err(Error::msg)?)?
        }
        FrontierCommands::Prefer { model, clear } => println!(
            "{}",
            ops::prefer(Vendor::Frontier, model.as_deref(), clear).map_err(Error::msg)?
        ),
        FrontierCommands::Probe { model, prompt } => {
            print_json(&ops::probe("frontier", Some(&model), &prompt).map_err(Error::msg)?)?
        }
    }
    Ok(())
}
