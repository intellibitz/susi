//! Deterministic AutoGen (AgentChat) control plane (no daemon required).
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    autogen_bind_workspace_config, autogen_doctor_or_bail, autogen_ensure_process_banner,
    autogen_init_workspace, autogen_setup_report, AgentManager, AutoGenDoctor, CatalogKind,
    RunStatus, AUTOGEN_CONFIG_ENV, AUTOGEN_ENGINE_ID,
};

#[derive(Debug, Subcommand)]
pub enum AutoGenCommands {
    /// Show catalog entry + readiness snapshot
    List,
    /// Check python, `autogen_agentchat` import, config, and API credentials
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write `.susi/autogen/{agent.py,config.json}` and print export hint
    Init,
    /// Start an AutoGen task (same lifecycle as `susi frameworks run autogen`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable AutoGen tasks in this workspace
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

fn python_from_adapter(manager: &AgentManager) -> Result<String> {
    match manager.adapter(AUTOGEN_ENGINE_ID)? {
        susi_agents::external::Adapter::Python { python, .. } => Ok(python),
        _ => bail!("expected python adapter for autogen"),
    }
}

pub fn execute(action: Option<AutoGenCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(AutoGenCommands::List);
    let manager = AgentManager::frameworks(workspace)?;
    match action {
        AutoGenCommands::List => {
            let def = susi_agents::external::definition(CatalogKind::Framework, AUTOGEN_ENGINE_ID)?;
            autogen_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            print_json(&serde_json::json!({
                "definition": def,
                "doctor": AutoGenDoctor::run(&python).to_json(),
            }))?;
        }
        AutoGenCommands::Doctor => {
            autogen_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            let detail = autogen_doctor_or_bail(&python)?;
            print_json(&serde_json::json!({
                "engine": AUTOGEN_ENGINE_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        AutoGenCommands::Setup => {
            autogen_bind_workspace_config(workspace);
            let adapter = manager.adapter(AUTOGEN_ENGINE_ID)?;
            print_json(&autogen_setup_report(&adapter)?)?;
        }
        AutoGenCommands::Init => print_json(&autogen_init_workspace(workspace)?)?,
        AutoGenCommands::Run { wait, prompt } => {
            autogen_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            autogen_doctor_or_bail(&python)?;
            let run = manager.prepare(AUTOGEN_ENGINE_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        AutoGenCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == AUTOGEN_ENGINE_ID)
                .collect();
            print_json(&runs)?;
        }
        AutoGenCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        AutoGenCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        AutoGenCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
        .args(["frameworks", "worker", id])
        .current_dir(&manager.read(id)?.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    autogen_ensure_process_banner(&mut command);
    if let Ok(v) = std::env::var(AUTOGEN_CONFIG_ENV) {
        command.env(AUTOGEN_CONFIG_ENV, v);
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
            "autogen task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
