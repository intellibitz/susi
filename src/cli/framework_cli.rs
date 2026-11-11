//! Deterministic agent-framework control plane (LangGraph, CrewAI, …), no daemon required.
use super::catalog_plane_cli::{launch, run_worker, LaunchOpts};
use crate::anyhow::{bail, Result};
use crate::cli_json::print_json;
use std::path::Path;
pub use susi_cli_defs::framework::FrameworkCommands;
use susi_daemon::adapter_ops::{self, AdapterKind};
use susi_gawd::external::{catalog, AgentManager, CatalogKind};

pub fn execute(action: FrameworkCommands, workspace: &Path) -> Result<()> {
    let manager = AgentManager::frameworks(workspace)?;
    match action {
        FrameworkCommands::List => print_json(&catalog(CatalogKind::Framework)?)?,
        // `susi_daemon::adapter_ops` builds the doctor/setup/configure/reset
        // documents and checks retries; the MCP `adapter_*` and
        // `*_retry` tools call the same functions.
        FrameworkCommands::Setup { engine } => print_json(
            &adapter_ops::setup(AdapterKind::Frameworks, &engine, workspace)
                .map_err(crate::anyhow::Error::msg)?,
        )?,
        FrameworkCommands::Doctor { engine } => {
            let rows = adapter_ops::doctor(AdapterKind::Frameworks, engine.as_deref(), workspace)
                .map_err(crate::anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!(
                    "{}",
                    adapter_ops::needs_setup_message(AdapterKind::Frameworks)
                );
            }
        }
        FrameworkCommands::Configure { engine, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Frameworks, &engine, doc, workspace)
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
        }
        FrameworkCommands::Reset { engine } => print_json(
            &adapter_ops::reset(AdapterKind::Frameworks, &engine, workspace)
                .map_err(crate::anyhow::Error::msg)?,
        )?,
        FrameworkCommands::Run {
            engine,
            wait,
            prompt,
        } => {
            let run = manager.prepare(&engine, &prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["frameworks", "worker", &run.id],
                    banner: None,
                    fail_label: "framework",
                },
            )?;
        }
        FrameworkCommands::Retry { task_id, wait } => {
            let old =
                adapter_ops::retry_source(&manager, &task_id).map_err(crate::anyhow::Error::msg)?;
            let run = manager.prepare(&old.agent, &old.prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["frameworks", "worker", &run.id],
                    banner: None,
                    fail_label: "framework",
                },
            )?;
        }
        FrameworkCommands::Worker { task_id } => run_worker(manager, &task_id, "framework")?,
        FrameworkCommands::Tasks => print_json(&manager.list()?)?,
        FrameworkCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        FrameworkCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => print!("{}", manager.logs(&task_id, stderr, bytes)?),
        FrameworkCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
    }
    Ok(())
}
