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
    let models = opening_model_inventory(&SusiDirs::config_dir(), &SusiDirs::data_dir());
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

/// A bounded filename inventory, not model admission or recursive discovery.
/// Installed channels reuse existing model directories through these paths.
fn opening_model_inventory(
    config_home: &std::path::Path,
    data_home: &std::path::Path,
) -> Vec<String> {
    let mut roots = vec![data_home.join("models")];
    if let Some(config) = std::fs::read_to_string(config_home.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    {
        roots.extend(
            config
                .get("local_scan_paths")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .take(16)
                .map(std::path::PathBuf::from),
        );
    }
    let mut models = std::collections::BTreeSet::new();
    for root in roots {
        for entry in std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .take(128)
            .flatten()
        {
            if entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
            {
                if let Some(name) = entry.file_name().to_str() {
                    models.insert(name.to_owned());
                }
            }
            if models.len() >= 128 {
                break;
            }
        }
        if models.len() >= 128 {
            break;
        }
    }
    models.into_iter().collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn opening_model_inventory_includes_reused_paths_without_recursing(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "susi-opening-inventory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let shared = root.join("shared");
        std::fs::create_dir_all(root.join("models"))?;
        std::fs::create_dir_all(shared.join("nested"))?;
        std::fs::write(root.join("models/own.gguf"), b"fixture")?;
        std::fs::write(shared.join("reused.GGUF"), b"fixture")?;
        std::fs::write(shared.join("nested/not-scanned.gguf"), b"fixture")?;
        std::fs::write(shared.join("ignore.json"), b"{}")?;
        std::fs::write(
            root.join("config.json"),
            serde_json::to_vec(
                &serde_json::json!({"local_scan_paths": [shared, shared, root.join("absent")]}),
            )?,
        )?;
        assert_eq!(
            super::opening_model_inventory(&root, &root),
            vec!["own.gguf", "reused.GGUF"]
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
