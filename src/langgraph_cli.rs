//! Deterministic LangGraph control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    langgraph_bind_workspace_config, langgraph_doctor_or_bail, langgraph_ensure_process_banner,
    langgraph_init_workspace, langgraph_setup_report, AgentManager, CatalogKind, LangGraphDoctor,
    RunStatus, LANGGRAPH_CONFIG_ENV, LANGGRAPH_ENGINE_ID,
};

#[derive(Debug, Subcommand)]
pub enum LangGraphCommands {
    /// Show catalog entry + readiness snapshot
    List,
    /// Check python, `langgraph` import, and SUSI_LANGGRAPH_CONFIG
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write `.susi/langgraph/{graph.py,config.json}` and print export hint
    Init,
    /// Start a LangGraph task (same lifecycle as `susi frameworks run langgraph`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable LangGraph tasks in this workspace
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

fn python_from_adapter(manager: &AgentManager) -> Result<String> {
    match manager.adapter(LANGGRAPH_ENGINE_ID)? {
        susi_agents::external::Adapter::Python { python, .. } => Ok(python),
        _ => bail!("expected python adapter for langgraph"),
    }
}

pub fn execute(action: Option<LangGraphCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(LangGraphCommands::List);
    let manager = AgentManager::frameworks(workspace)?;
    match action {
        LangGraphCommands::List => {
            let def =
                susi_agents::external::definition(CatalogKind::Framework, LANGGRAPH_ENGINE_ID)?;
            langgraph_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            print_json(&serde_json::json!({
                "definition": def,
                "doctor": LangGraphDoctor::run(&python).to_json(),
            }))?;
        }
        LangGraphCommands::Doctor => {
            langgraph_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            let detail = langgraph_doctor_or_bail(&python)?;
            print_json(&serde_json::json!({
                "engine": LANGGRAPH_ENGINE_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        LangGraphCommands::Setup => {
            langgraph_bind_workspace_config(workspace);
            let adapter = manager.adapter(LANGGRAPH_ENGINE_ID)?;
            print_json(&langgraph_setup_report(&adapter)?)?;
        }
        LangGraphCommands::Init => print_json(&langgraph_init_workspace(workspace)?)?,
        LangGraphCommands::Run { wait, prompt } => {
            langgraph_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            langgraph_doctor_or_bail(&python)?;
            let run = manager.prepare(LANGGRAPH_ENGINE_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        LangGraphCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == LANGGRAPH_ENGINE_ID)
                .collect();
            print_json(&runs)?;
        }
        LangGraphCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        LangGraphCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        LangGraphCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
    langgraph_ensure_process_banner(&mut command);
    // Propagate config so the detached worker sees the same binding.
    if let Ok(v) = std::env::var(LANGGRAPH_CONFIG_ENV) {
        command.env(LANGGRAPH_CONFIG_ENV, v);
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
            "langgraph task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
