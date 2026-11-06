//! Deterministic OpenRouter control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{Error, Result};

pub use susi_cli_defs::openrouter::*;

pub fn execute(action: Option<OpenRouterCommands>) -> Result<()> {
    // `susi_daemon::provider_ops` builds every document; the MCP
    // `provider_*` tools return the same ones.
    use susi_daemon::provider_ops::{self as ops, Vendor};
    let action = action.unwrap_or(OpenRouterCommands::List);
    match action {
        OpenRouterCommands::List => {
            print_json(&ops::list(Vendor::OpenRouter).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Doctor { live } => {
            for doc in ops::doctor(Vendor::OpenRouter, None, live).map_err(Error::msg)? {
                print_json(&doc)?;
            }
        }
        OpenRouterCommands::Setup => {
            print_json(&ops::setup(Vendor::OpenRouter, None).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Prefer { model, clear_model } => println!(
            "{}",
            ops::prefer(Vendor::OpenRouter, model.as_deref(), clear_model).map_err(Error::msg)?
        ),
        OpenRouterCommands::Probe { prompt } => {
            print_json(&ops::probe("openrouter", None, &prompt).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Models { live, limit } => {
            print_json(&ops::openrouter_models(live, limit).map_err(Error::msg)?)?
        }
    }
    Ok(())
}
