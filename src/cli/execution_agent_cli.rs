//! Shared clap control plane for execution agents (aider, openhands, …).

use super::catalog_plane_cli::{launch, LaunchOpts};
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use std::path::Path;
use susi_agents::external::{AgentManager, CatalogKind};

/// Hooks into a single catalog execution-agent plane.
pub struct ExecutionAgentPlane {
    pub agent_id: &'static str,
    /// Optional `SUSI_PROCESS_BANNER` for detached worker processes.
    pub process_banner: Option<&'static str>,
    pub status: fn() -> serde_json::Value,
    pub doctor: fn() -> Result<String>,
    pub setup: fn() -> serde_json::Value,
}

#[derive(Debug, Subcommand)]
pub enum ExecutionAgentCommands {
    /// Show agent binary/auth status and catalog definition
    List,
    /// Check prerequisites (does not start a paid run)
    Doctor,
    /// Show install/auth instructions and effective adapter
    Setup,
    /// Start a headless task (same lifecycle as `susi agents run <id>`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable tasks for this agent in this workspace
    Tasks,
    Status {
        task_id: String,
        #[arg(long)]
        refresh: bool,
    },
    Logs {
        task_id: String,
        #[arg(long)]
        stderr: bool,
        #[arg(long, default_value_t = 65536)]
        bytes: u64,
    },
    Cancel {
        task_id: String,
    },
}

pub fn execute(
    plane: &ExecutionAgentPlane,
    action: Option<ExecutionAgentCommands>,
    workspace: &Path,
) -> Result<()> {
    let action = action.unwrap_or(ExecutionAgentCommands::List);
    let manager = AgentManager::new(workspace)?;
    let agent_id = plane.agent_id;
    match action {
        ExecutionAgentCommands::List => {
            let def = susi_agents::external::definition(CatalogKind::Execution, agent_id)?;
            print_json(&serde_json::json!({
                "status": (plane.status)(),
                "definition": def,
            }))?;
        }
        ExecutionAgentCommands::Doctor => {
            let detail = (plane.doctor)()?;
            print_json(&serde_json::json!({
                "agent": agent_id,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        ExecutionAgentCommands::Setup => print_json(&(plane.setup)())?,
        ExecutionAgentCommands::Run { wait, prompt } => {
            (plane.doctor)()?;
            let run = manager.prepare(agent_id, &prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["agents", "worker", &run.id],
                    banner: plane.process_banner,
                    fail_label: agent_id,
                },
            )?;
        }
        ExecutionAgentCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == agent_id)
                .collect();
            print_json(&runs)?;
        }
        ExecutionAgentCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        ExecutionAgentCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        ExecutionAgentCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
    }
    Ok(())
}
