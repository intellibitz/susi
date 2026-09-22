//! Shared clap control plane for execution agents (aider, openhands, …).

use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{AgentManager, CatalogKind, RunStatus};

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
            launch(&manager, &run.id, wait, plane.process_banner, agent_id)?;
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

fn launch(
    manager: &AgentManager,
    id: &str,
    wait: bool,
    banner: Option<&str>,
    agent_id: &str,
) -> Result<()> {
    print_json(&manager.read(id)?)?;
    if wait {
        return run_worker(manager.clone(), id, agent_id);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["agents", "worker", id])
        .current_dir(&manager.read(id)?.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(banner) = banner {
        if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
            command.env("SUSI_PROCESS_BANNER", banner);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200);
    }
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            manager.mark_launch_failed(id, &e.to_string())?;
            return Err(e.into());
        }
    }
    Ok(())
}

fn run_worker(manager: AgentManager, id: &str, agent_id: &str) -> Result<()> {
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, shutdown.clone())?;
    }
    let run = manager.with_shutdown(shutdown).execute(id)?;
    print_json(&run)?;
    if matches!(
        run.status,
        RunStatus::Failed | RunStatus::Unknown | RunStatus::Cancelled
    ) {
        bail!(
            "{agent_id} task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
