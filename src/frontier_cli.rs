//! Deterministic top frontier model control plane (no daemon for list/doctor/setup).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use susi_gemi::frontier::{FrontierManager, FrontierOverride};

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
    let action = action.unwrap_or(FrontierCommands::List);
    let manager = FrontierManager::new()?;
    match action {
        FrontierCommands::List => {
            let preferred = manager.preferred();
            let rows: Vec<_> = FrontierManager::catalog()?
                .into_iter()
                .map(|m| {
                    let effective = manager.effective(&m.id).unwrap_or(m.clone());
                    let readiness = manager.preflight(&m.id);
                    serde_json::json!({
                        "model": effective,
                        "preferred": preferred.as_deref() == Some(m.id.as_str()),
                        "prerequisites_present": readiness.is_ok(),
                        "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "status": manager.status(),
                "models": rows,
            }))?;
        }
        FrontierCommands::Doctor { model } => {
            let models = match model {
                Some(id) => vec![FrontierManager::definition(&id)?],
                None => FrontierManager::catalog()?,
            };
            let mut missing = false;
            for m in models {
                let result = manager.preflight(&m.id);
                missing |= result.is_err();
                print_json(&serde_json::json!({
                    "model": m.id,
                    "prerequisites_present": result.is_ok(),
                    "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                }))?;
            }
            if missing {
                bail!("one or more frontier models need setup");
            }
        }
        FrontierCommands::Setup { model } => print_json(&manager.setup(&model)?)?,
        FrontierCommands::Configure { model, file } => {
            let over: FrontierOverride = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&manager.configure(&model, &over)?)?;
        }
        FrontierCommands::Reset { model } => print_json(&manager.reset(&model)?)?,
        FrontierCommands::Prefer { model, clear } => {
            if clear {
                println!("{}", susi_gemi::frontier_ext::prefer("", true)?);
            } else {
                let Some(model) = model.filter(|s| !s.trim().is_empty()) else {
                    bail!("model id required (or pass --clear)");
                };
                println!("{}", susi_gemi::frontier_ext::prefer(&model, false)?);
            }
        }
        FrontierCommands::Probe { model, prompt } => {
            let output = susi_gemi::frontier_ext::probe(&model, &prompt)?;
            let def = manager.effective(&model)?;
            print_json(&serde_json::json!({
                "model": def.id,
                "api_id": def.model,
                "engine": def.engine,
                "output": output,
            }))?;
        }
    }
    Ok(())
}
