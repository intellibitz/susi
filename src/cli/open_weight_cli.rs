//! Deterministic open-weight frontier model control plane (local host; no daemon for list/doctor).
use crate::cli_json::print_json;
use anyhow::{Error, Result};

pub use susi_cli_defs::open_weight::*;

pub fn execute(action: Option<OpenWeightCommands>) -> Result<()> {
    // `susi_daemon::provider_ops` builds every document; the MCP
    // `provider_*` tools return the same ones.
    use susi_daemon::provider_ops::{self as ops, Vendor};
    let action = action.unwrap_or(OpenWeightCommands::List);
    match action {
        OpenWeightCommands::List => {
            print_json(&ops::list(Vendor::OpenWeight).map_err(Error::msg)?)?
        }
        OpenWeightCommands::Doctor { model } => {
            for doc in
                ops::doctor(Vendor::OpenWeight, model.as_deref(), false).map_err(Error::msg)?
            {
                print_json(&doc)?;
            }
        }
        OpenWeightCommands::Setup { model } => {
            print_json(&ops::setup(Vendor::OpenWeight, Some(&model)).map_err(Error::msg)?)?
        }
        OpenWeightCommands::Pull { model } => {
            println!("{}", ops::pull(&model).map_err(Error::msg)?)
        }
        OpenWeightCommands::Configure { model, file } => {
            let over: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&ops::configure(Vendor::OpenWeight, &model, over).map_err(Error::msg)?)?;
        }
        OpenWeightCommands::Reset { model } => {
            print_json(&ops::reset(Vendor::OpenWeight, &model).map_err(Error::msg)?)?
        }
        OpenWeightCommands::Prefer { model, clear } => println!(
            "{}",
            ops::prefer(Vendor::OpenWeight, model.as_deref(), clear).map_err(Error::msg)?
        ),
        OpenWeightCommands::Probe { model, prompt } => {
            print_json(&ops::probe("openweight", Some(&model), &prompt).map_err(Error::msg)?)?
        }
    }
    Ok(())
}
