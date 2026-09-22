//! Deterministic OpenViking control plane (no daemon required).
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    openviking_doctor, openviking_ensure_process_banner, openviking_init_local_client,
    openviking_setup, openviking_status, AgentManager, CatalogKind, RunStatus, OPENVIKING_AGENT_ID,
    OPENVIKING_DEFAULT_LOCAL_URL,
};

#[derive(Debug, Subcommand)]
pub enum OpenVikingCommands {
    /// Show OpenViking CLI + server/health status
    List,
    /// Check `ov` binary, client config, and endpoint health
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write/activate a local client config (`ov config add custom …`)
    Init {
        /// OpenViking server URL
        #[arg(long, default_value = OPENVIKING_DEFAULT_LOCAL_URL)]
        url: String,
        /// Saved config name
        #[arg(long, default_value = "susi")]
        name: String,
    },
    /// Run semantic retrieval (`ov find`) as a durable task
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable OpenViking tasks in this workspace
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

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

pub fn execute(action: Option<OpenVikingCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(OpenVikingCommands::List);
    let manager = AgentManager::new(workspace)?;
    match action {
        OpenVikingCommands::List => {
            let def =
                susi_agents::external::definition(CatalogKind::Execution, OPENVIKING_AGENT_ID)?;
            print_json(&serde_json::json!({
                "status": openviking_status(),
                "definition": def,
            }))?;
        }
        OpenVikingCommands::Doctor => {
            let detail = openviking_doctor()?;
            print_json(&serde_json::json!({
                "agent": OPENVIKING_AGENT_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        OpenVikingCommands::Setup => print_json(&openviking_setup())?,
        OpenVikingCommands::Init { url, name } => {
            print_json(&openviking_init_local_client(&url, &name)?)?
        }
        OpenVikingCommands::Run { wait, prompt } => {
            openviking_doctor()?;
            let run = manager.prepare(OPENVIKING_AGENT_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        OpenVikingCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == OPENVIKING_AGENT_ID)
                .collect();
            print_json(&runs)?;
        }
        OpenVikingCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        OpenVikingCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        OpenVikingCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
    openviking_ensure_process_banner(&mut command);
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
            "openviking task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
