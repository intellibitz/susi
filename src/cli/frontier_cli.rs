//! Deterministic top frontier model control plane (no daemon for list/doctor/setup).
use crate::anyhow::{bail, Error, Result};
use crate::cli_json::print_json;
pub use susi_cli_defs::frontier::FrontierCommands;

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
