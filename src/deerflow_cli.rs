//! Deterministic DeerFlow control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    deerflow_apply_process_env, deerflow_bind_workspace_config, deerflow_doctor,
    deerflow_init_workspace, deerflow_setup, deerflow_status, AgentManager, CatalogKind, RunStatus,
    DEERFLOW_AGENT_ID,
};

#[derive(Debug, Subcommand)]
pub enum DeerFlowCommands {
    /// Show DeerFlow CLI + config/credential status
    List,
    /// Check `deerflow` binary, config.yaml, and model API credentials
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write `.susi/deerflow/{config.yaml,home/}` and print export hints
    Init,
    /// Start a headless DeerFlow task (same lifecycle as `susi agents run deerflow`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable DeerFlow tasks in this workspace
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

pub fn execute(action: Option<DeerFlowCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(DeerFlowCommands::List);
    let manager = AgentManager::new(workspace)?;
    match action {
        DeerFlowCommands::List => {
            deerflow_bind_workspace_config(workspace);
            let def = susi_agents::external::definition(CatalogKind::Execution, DEERFLOW_AGENT_ID)?;
            print_json(&serde_json::json!({
                "status": deerflow_status(),
                "definition": def,
            }))?;
        }
        DeerFlowCommands::Doctor => {
            deerflow_bind_workspace_config(workspace);
            let detail = deerflow_doctor()?;
            print_json(&serde_json::json!({
                "agent": DEERFLOW_AGENT_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        DeerFlowCommands::Setup => {
            deerflow_bind_workspace_config(workspace);
            print_json(&deerflow_setup())?
        }
        DeerFlowCommands::Init => print_json(&deerflow_init_workspace(workspace)?)?,
        DeerFlowCommands::Run { wait, prompt } => {
            deerflow_bind_workspace_config(workspace);
            deerflow_doctor()?;
            let run = manager.prepare(DEERFLOW_AGENT_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        DeerFlowCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == DEERFLOW_AGENT_ID)
                .collect();
            print_json(&runs)?;
        }
        DeerFlowCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        DeerFlowCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        DeerFlowCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
    }
    Ok(())
}

fn launch(manager: &AgentManager, id: &str, wait: bool) -> Result<()> {
    print_json(&manager.read(id)?)?;
    if wait {
        return run_worker(manager.clone(), id);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["agents", "worker", id])
        .current_dir(&manager.read(id)?.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    deerflow_apply_process_env(&mut command);
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

fn run_worker(manager: AgentManager, id: &str) -> Result<()> {
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
            "deerflow task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
