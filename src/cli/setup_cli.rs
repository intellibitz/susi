//! `susi setup`: one end-to-end zero-config ecosystem setup command.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use susi_config::setup_workflow::{ConsentKind, SetupPlan};
use susi_paths::SusiDirs;

#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Detect hardware/engines/keys/agents and print the plan (default)
    Plan {
        /// Machine-readable plan for automation
        #[arg(long)]
        json: bool,
    },
    /// Apply the plan (optionally grant cloud consent)
    Apply {
        /// Grant cloud API key consent in this apply
        #[arg(long)]
        cloud: bool,
        /// Machine-readable result for automation
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(action: Option<SetupCommands>) -> Result<()> {
    let home = SusiDirs::config_dir();
    match action.unwrap_or(SetupCommands::Plan { json: false }) {
        SetupCommands::Plan { json } => {
            let plan = SetupPlan::detect(&home);
            if json {
                print_json(&serde_json::to_value(&plan)?)?;
            } else {
                print_plan(&plan);
            }
        }
        SetupCommands::Apply { cloud, json } => {
            let mut plan = SetupPlan::detect(&home);
            if cloud {
                plan.grant_consent(ConsentKind::CloudApiKey);
            }
            let summary = plan.apply(&home).map_err(anyhow::Error::msg)?;
            if json {
                print_json(&serde_json::json!({
                    "hardware": plan.hardware,
                    "engines": plan.engines,
                    "keys_present": plan.keys_present,
                    "agents": plan.agents,
                    "summary": summary,
                }))?;
            } else {
                println!("Susi's defaults are installed. Existing settings were preserved.");
                print_plan(&plan);
            }
        }
    }
    Ok(())
}

fn print_plan(plan: &SetupPlan) {
    println!("Susi — give me a goal.");
    if plan.keys_present.is_empty() {
        println!("Cloud: no existing provider connection found.");
    } else {
        let names: Vec<_> = plan
            .keys_present
            .iter()
            .map(|key| key.trim_end_matches("_API_KEY"))
            .collect();
        println!(
            "Cloud: connections found for {} (availability checked when used).",
            names.join(", ")
        );
    }
    let cached = std::fs::read_to_string(SusiDirs::config_dir().join("local_ecosystem.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let engines: Vec<_> = cached
        .as_ref()
        .and_then(|value| value.get("engines"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|engine| engine.get("running").and_then(serde_json::Value::as_bool) == Some(true))
        .filter_map(|engine| engine.get("name").and_then(serde_json::Value::as_str))
        .collect();
    if engines.is_empty() {
        println!("Local AI: not yet verified.");
    } else {
        println!(
            "Local AI: previously available {} (cached observation).",
            engines.join(", ")
        );
    }
    let models: Vec<_> = std::fs::read_dir(SusiDirs::data_dir().join("models"))
        .into_iter()
        .flatten()
        .take(128)
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "gguf")
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    if !models.is_empty() {
        println!(
            "Local models found: {} (loading checked when used).",
            models.join(", ")
        );
    }
    if !plan.agents.is_empty() {
        println!(
            "Previously discovered agents: {} (checked when used).",
            plan.agents.join(", ")
        );
    }
    println!("Optional tools and agents connect when needed. Type your goal to begin.");
}
