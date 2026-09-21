//! Deterministic coding/agent model control plane (no daemon required for list/doctor/setup).
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_gemi::coding_models::{CodingModelManager, CodingModelOverride};

#[derive(Debug, Subcommand)]
pub enum ModelCommands {
    /// Show the top developer/agent-focused models and readiness
    List,
    /// Check credentials/endpoints (does not call a paid model)
    Doctor { model: Option<String> },
    /// Show setup instructions and the effective model wiring
    Setup { model: String },
    /// Override engine/model/protocol/api_key_env via JSON file
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled coding-model entry
    Reset { model: String },
    /// Prefer a coding model for subsequent routing (`selected_model_override`)
    Prefer { model: String },
    /// Paid probe: send a short completion to verify auth and model id
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
    /// List local/scanned GGUF models (legacy mission path)
    Local,
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

pub fn execute(action: Option<ModelCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(ModelCommands::List);
    let manager = CodingModelManager::new()?;
    match action {
        ModelCommands::List => {
            let preferred = manager.preferred();
            let rows: Vec<_> = CodingModelManager::catalog()?
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
            print_json(&rows)?;
        }
        ModelCommands::Doctor { model } => {
            let models = match model {
                Some(id) => vec![CodingModelManager::definition(&id)?],
                None => CodingModelManager::catalog()?,
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
                bail!("one or more coding models need setup");
            }
        }
        ModelCommands::Setup { model } => {
            let def = manager.effective(&model)?;
            print_json(&serde_json::json!({
                "model": def,
                "preferred": manager.preferred(),
                "instructions": "Set the vendor key with `susi keys set <engine>` (or export api_key_env). API model ids drift — override with `susi models configure`. Run doctor, then optional probe (paid). Prefer pins routing. No subscriptions are provisioned implicitly."
            }))?;
        }
        ModelCommands::Configure { model, file } => {
            let over: CodingModelOverride = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&manager.configure(&model, &over)?)?;
        }
        ModelCommands::Reset { model } => print_json(&manager.reset(&model)?)?,
        ModelCommands::Prefer { model } => {
            println!("{}", manager.prefer(&model)?);
        }
        ModelCommands::Probe { model, prompt } => {
            let output = susi_gemi::coding_models_ext::probe(&model, &prompt)?;
            print_json(&serde_json::json!({
                "model": model,
                "output": output
            }))?;
        }
        ModelCommands::Local => {
            // Handled by caller with AMA when returning LocalNeedsMission — keep pure here.
            let _ = workspace;
            bail!("LOCAL_MODELS_MISSION");
        }
    }
    Ok(())
}
