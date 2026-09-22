//! Deterministic open-weight frontier model control plane (local host; no daemon for list/doctor).
use anyhow::{bail, Result};
use clap::Subcommand;
use susi_gemi::open_weight::{OpenWeightManager, OpenWeightOverride};

#[derive(Debug, Subcommand)]
pub enum OpenWeightCommands {
    /// Show the top open-weight frontier models and readiness
    List,
    /// Check local endpoint / pull status (does not download weights)
    Doctor { model: Option<String> },
    /// Show setup instructions and effective wiring
    Setup { model: String },
    /// Pull weights via `ollama pull <tag>`
    Pull { model: String },
    /// Override engine/ollama_tag/protocol via JSON file
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled entry
    Reset { model: String },
    /// Prefer an open-weight model for local routing
    Prefer {
        model: Option<String>,
        /// Clear the preferred open-weight pin
        #[arg(long)]
        clear: bool,
    },
    /// Local probe: short completion via Ollama/vLLM
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

pub fn execute(action: Option<OpenWeightCommands>) -> Result<()> {
    let action = action.unwrap_or(OpenWeightCommands::List);
    let manager = OpenWeightManager::new()?;
    match action {
        OpenWeightCommands::List => {
            let preferred = manager.preferred();
            let rows: Vec<_> = OpenWeightManager::catalog()?
                .into_iter()
                .map(|m| {
                    let effective = manager.effective(&m.id).unwrap_or(m.clone());
                    let readiness = manager.doctor(Some(&m.id));
                    let pulled = manager.model_pulled(&effective.ollama_tag).unwrap_or(false);
                    serde_json::json!({
                        "model": effective,
                        "preferred": preferred.as_deref() == Some(m.id.as_str()),
                        "prerequisites_present": readiness.is_ok(),
                        "pulled": pulled,
                        "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "status": manager.status(),
                "models": rows,
            }))?;
        }
        OpenWeightCommands::Doctor { model } => {
            let detail = manager.doctor(model.as_deref())?;
            print_json(&serde_json::json!({
                "vendor": "open-weight",
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        OpenWeightCommands::Setup { model } => print_json(&manager.setup(&model)?)?,
        OpenWeightCommands::Pull { model } => {
            println!("{}", manager.pull(&model)?);
        }
        OpenWeightCommands::Configure { model, file } => {
            let over: OpenWeightOverride = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&manager.configure(&model, &over)?)?;
        }
        OpenWeightCommands::Reset { model } => print_json(&manager.reset(&model)?)?,
        OpenWeightCommands::Prefer { model, clear } => {
            if clear {
                println!("{}", susi_gemi::open_weight_ext::prefer("", true)?);
            } else {
                let Some(model) = model.filter(|s| !s.trim().is_empty()) else {
                    bail!("model id required (or pass --clear)");
                };
                println!("{}", susi_gemi::open_weight_ext::prefer(&model, false)?);
            }
        }
        OpenWeightCommands::Probe { model, prompt } => {
            let output = susi_gemi::open_weight_ext::probe(&model, &prompt)?;
            let def = manager.effective(&model)?;
            print_json(&serde_json::json!({
                "model": def.id,
                "ollama_tag": def.ollama_tag,
                "output": output,
            }))?;
        }
    }
    Ok(())
}
