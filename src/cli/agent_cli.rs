//! Deterministic external-agent control plane, available without inference/daemon boot.
use super::catalog_plane_cli::{launch, run_worker, LaunchOpts};
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use std::path::Path;
use susi_gawd::external::{catalog, AgentManager, CatalogKind};
pub use susi_cli_defs::agent::AgentCommands;
use susi_daemon::adapter_ops::{self, AdapterKind};

pub fn execute(action: AgentCommands, workspace: &Path) -> Result<()> {
    let manager = AgentManager::new(workspace)?;
    match action {
        AgentCommands::List => print_json(&catalog(CatalogKind::Execution)?)?,
        // `susi_daemon::adapter_ops` builds the doctor/setup/configure/reset
        // documents and checks retries; the MCP `adapter_*` and
        // `*_retry` tools call the same functions.
        AgentCommands::Setup { agent } => print_json(
            &adapter_ops::setup(AdapterKind::Agents, &agent, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        AgentCommands::Doctor { agent } => {
            let rows = adapter_ops::doctor(AdapterKind::Agents, agent.as_deref(), workspace)
                .map_err(anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!("{}", adapter_ops::needs_setup_message(AdapterKind::Agents));
            }
        }
        AgentCommands::Configure { agent, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Agents, &agent, doc, workspace)
                    .map_err(anyhow::Error::msg)?,
            )?;
        }
        AgentCommands::Reset { agent } => print_json(
            &adapter_ops::reset(AdapterKind::Agents, &agent, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        AgentCommands::Run {
            agent,
            wait,
            prompt,
        } => {
            let run = manager.prepare(&agent, &prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["agents", "worker", &run.id],
                    banner: None,
                    fail_label: "agent",
                },
            )?;
        }
        AgentCommands::Retry { task_id, wait } => {
            let old = adapter_ops::retry_source(&manager, &task_id).map_err(anyhow::Error::msg)?;
            let run = manager.prepare(&old.agent, &old.prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["agents", "worker", &run.id],
                    banner: None,
                    fail_label: "agent",
                },
            )?;
        }
        AgentCommands::Worker { task_id } => run_worker(manager, &task_id, "agent")?,
        AgentCommands::Tasks => print_json(&manager.list()?)?,
        AgentCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        AgentCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => print!("{}", manager.logs(&task_id, stderr, bytes)?),
        AgentCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
        AgentCommands::Send { task_id, message } => print_json(&manager.send(&task_id, &message)?)?,
    }
    Ok(())
}
