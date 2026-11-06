//! Deterministic coding/agent model control plane (no daemon required for list/doctor/setup).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use std::path::Path;
use susi_daemon::adapter_ops::{self, AdapterKind};
use susi_gemi::models::coding_models::CodingModelManager;

pub use susi_cli_defs::model::*;

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
        // `susi_daemon::adapter_ops` builds these documents; the MCP
        // `adapter_*` tools return the same ones.
        ModelCommands::Doctor { model } => {
            let rows = adapter_ops::doctor(AdapterKind::Models, model.as_deref(), workspace)
                .map_err(anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!("{}", adapter_ops::needs_setup_message(AdapterKind::Models));
            }
        }
        ModelCommands::Setup { model } => print_json(
            &adapter_ops::setup(AdapterKind::Models, &model, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        ModelCommands::Configure { model, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Models, &model, doc, workspace)
                    .map_err(anyhow::Error::msg)?,
            )?;
        }
        ModelCommands::Reset { model } => print_json(
            &adapter_ops::reset(AdapterKind::Models, &model, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        ModelCommands::Prefer { model } => {
            println!("{}", manager.prefer(&model)?);
        }
        ModelCommands::Probe { model, prompt } => print_json(
            &susi_daemon::provider_ops::probe("coding", Some(&model), &prompt)
                .map_err(anyhow::Error::msg)?,
        )?,
    }
    Ok(())
}
