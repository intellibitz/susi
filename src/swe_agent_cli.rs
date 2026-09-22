//! Deterministic SWE-agent control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    swe_agent_doctor, swe_agent_setup, swe_agent_status, AgentManager, CatalogKind, RunStatus,
    SWE_AGENT_ID,
};

#[derive(Debug, Subcommand)]
pub enum SweAgentCommands {
    /// Show SWE-agent binary + LLM/Docker status
    List,
    /// Check CLI binary and credentials (does not start a paid run)
    Doctor,
    /// Show install/auth instructions and effective adapter
    Setup,
    /// Start a headless SWE-agent task (same lifecycle as `susi agents run swe-agent`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable SWE-agent tasks in this workspace
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

pub fn execute(action: Option<SweAgentCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(SweAgentCommands::List);
    let manager = AgentManager::new(workspace)?;
    match action {
        SweAgentCommands::List => {
            let def = susi_agents::external::definition(CatalogKind::Execution, SWE_AGENT_ID)?;
            print_json(&serde_json::json!({
                "status": swe_agent_status(),
                "definition": def,
            }))?;
        }
        SweAgentCommands::Doctor => {
            let detail = swe_agent_doctor()?;
            print_json(&serde_json::json!({
                "agent": SWE_AGENT_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        SweAgentCommands::Setup => print_json(&swe_agent_setup())?,
        SweAgentCommands::Run { wait, prompt } => {
            swe_agent_doctor()?;
            let run = manager.prepare(SWE_AGENT_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        SweAgentCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == SWE_AGENT_ID)
                .collect();
            print_json(&runs)?;
        }
        SweAgentCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        SweAgentCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        SweAgentCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        command.env("SUSI_PROCESS_BANNER", "susi-swe-agent");
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
            "swe-agent task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
