//! Deterministic external-agent control plane, available without inference/daemon boot.
use super::catalog_plane_cli::{launch, run_worker, LaunchOpts};
use crate::anyhow::{bail, Result};
use crate::cli_json::print_json;
use std::path::Path;
pub use susi_cli_defs::agent::AgentCommands;
use susi_daemon::adapter_ops::{self, AdapterKind};
use susi_gawd::external::{catalog, AgentManager, CatalogKind};

pub fn execute(action: AgentCommands, workspace: &Path) -> Result<()> {
    execute_catalog(action, workspace, CatalogKind::Execution)
}

/// Agent and framework commands share execution; catalog identity still
/// controls configuration, durable tasks, worker argv and diagnostics.
pub(super) fn execute_catalog(
    action: AgentCommands,
    workspace: &Path,
    kind: CatalogKind,
) -> Result<()> {
    let manager = AgentManager::for_kind(workspace, kind)?;
    let (adapter, command) = match kind {
        CatalogKind::Execution => (AdapterKind::Agents, "agents"),
        CatalogKind::Framework => (AdapterKind::Frameworks, "frameworks"),
    };
    let label = kind.label();
    let launch_task = |agent: &str, prompt: &str, wait| {
        let run = manager.prepare(agent, prompt)?;
        launch(
            &manager,
            &run.id,
            LaunchOpts {
                wait,
                worker_argv: &[command, "worker", &run.id],
                banner: None,
                fail_label: label,
            },
        )
    };
    match action {
        AgentCommands::List => print_json(&catalog(kind)?)?,
        // `susi_daemon::adapter_ops` builds the doctor/setup/configure/reset
        // documents and checks retries; the MCP `adapter_*` and
        // `*_retry` tools call the same functions.
        AgentCommands::Setup { agent } => print_json(
            &adapter_ops::setup(adapter, &agent, workspace).map_err(crate::anyhow::Error::msg)?,
        )?,
        AgentCommands::Doctor { agent } => {
            let rows = adapter_ops::doctor(adapter, agent.as_deref(), workspace)
                .map_err(crate::anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!("{}", adapter_ops::needs_setup_message(adapter));
            }
        }
        AgentCommands::Configure { agent, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(adapter, &agent, doc, workspace)
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
        }
        AgentCommands::Reset { agent } => print_json(
            &adapter_ops::reset(adapter, &agent, workspace).map_err(crate::anyhow::Error::msg)?,
        )?,
        AgentCommands::Run {
            agent,
            wait,
            prompt,
        } => launch_task(&agent, &prompt, wait)?,
        AgentCommands::Retry { task_id, wait } => {
            let old =
                adapter_ops::retry_source(&manager, &task_id).map_err(crate::anyhow::Error::msg)?;
            launch_task(&old.agent, &old.prompt, wait)?;
        }
        AgentCommands::Worker { task_id } => run_worker(manager, &task_id, label)?,
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
