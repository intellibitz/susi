//! Deterministic OpenAI Agents SDK control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    openai_agents_bind_workspace_config, openai_agents_doctor_or_bail,
    openai_agents_ensure_process_banner, openai_agents_init_workspace, openai_agents_setup_report,
    AgentManager, CatalogKind, OpenAiAgentsDoctor, RunStatus, OPENAI_AGENTS_CONFIG_ENV,
    OPENAI_AGENTS_ENGINE_ID,
};

#[derive(Debug, Subcommand)]
pub enum OpenAiAgentsCommands {
    /// Show catalog entry + readiness snapshot
    List,
    /// Check python, `agents` import, config, and API credentials
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write `.susi/openai-agents/{agent.py,config.json}` and print export hint
    Init,
    /// Start an OpenAI Agents SDK task (same lifecycle as `susi frameworks run openai-agents`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable OpenAI Agents tasks in this workspace
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
    match manager.adapter(OPENAI_AGENTS_ENGINE_ID)? {
        susi_agents::external::Adapter::Python { python, .. } => Ok(python),
        _ => bail!("expected python adapter for openai-agents"),
    }
}

pub fn execute(action: Option<OpenAiAgentsCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(OpenAiAgentsCommands::List);
    let manager = AgentManager::frameworks(workspace)?;
    match action {
        OpenAiAgentsCommands::List => {
            let def =
                susi_agents::external::definition(CatalogKind::Framework, OPENAI_AGENTS_ENGINE_ID)?;
            openai_agents_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            print_json(&serde_json::json!({
                "definition": def,
                "doctor": OpenAiAgentsDoctor::run(&python).to_json(),
            }))?;
        }
        OpenAiAgentsCommands::Doctor => {
            openai_agents_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            let detail = openai_agents_doctor_or_bail(&python)?;
            print_json(&serde_json::json!({
                "engine": OPENAI_AGENTS_ENGINE_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        OpenAiAgentsCommands::Setup => {
            openai_agents_bind_workspace_config(workspace);
            let adapter = manager.adapter(OPENAI_AGENTS_ENGINE_ID)?;
            print_json(&openai_agents_setup_report(&adapter)?)?;
        }
        OpenAiAgentsCommands::Init => print_json(&openai_agents_init_workspace(workspace)?)?,
        OpenAiAgentsCommands::Run { wait, prompt } => {
            openai_agents_bind_workspace_config(workspace);
            let python = python_from_adapter(&manager)?;
            openai_agents_doctor_or_bail(&python)?;
            let run = manager.prepare(OPENAI_AGENTS_ENGINE_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        OpenAiAgentsCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == OPENAI_AGENTS_ENGINE_ID)
                .collect();
            print_json(&runs)?;
        }
        OpenAiAgentsCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        OpenAiAgentsCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        OpenAiAgentsCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
    openai_agents_ensure_process_banner(&mut command);
    if let Ok(v) = std::env::var(OPENAI_AGENTS_CONFIG_ENV) {
        command.env(OPENAI_AGENTS_CONFIG_ENV, v);
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
            "openai-agents task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
