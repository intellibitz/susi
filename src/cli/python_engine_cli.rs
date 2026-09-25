//! Shared clap control plane for Python agent engines (crewai, temporal, …).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    python_engine::{EngineDoctor, EngineProfile},
    AgentManager, CatalogKind, RunStatus,
};

#[derive(Debug, Subcommand)]
pub enum EngineCommands {
    /// Show catalog entry + readiness snapshot
    List,
    /// Check python, package import, config, and credentials
    Doctor,
    /// Show install/config instructions and effective adapter
    Setup,
    /// Write `.susi/<engine>/{agent.py,config.json}` and print export hint
    Init,
    /// Start a durable framework task
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable tasks for this engine in this workspace
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

#[allow(clippy::wildcard_enum_match_arm)] // only the Python framework adapter applies; other Adapter kinds are rejected
pub fn execute(
    profile: &EngineProfile,
    action: Option<EngineCommands>,
    workspace: &Path,
) -> Result<()> {
    let action = action.unwrap_or(EngineCommands::List);
    let manager = AgentManager::frameworks(workspace)?;
    let python = match manager.adapter(profile.engine_id)? {
        susi_agents::external::Adapter::Python { python, .. } => python,
        _ => bail!("expected python adapter for {}", profile.engine_id),
    };
    match action {
        EngineCommands::List => {
            let def = susi_agents::external::definition(CatalogKind::Framework, profile.engine_id)?;
            susi_agents::external::python_engine::bind_workspace_config(profile, workspace);
            print_json(&serde_json::json!({
                "definition": def,
                "doctor": EngineDoctor::run(profile, &python).to_json(),
            }))?;
        }
        EngineCommands::Doctor => {
            susi_agents::external::python_engine::bind_workspace_config(profile, workspace);
            let detail = susi_agents::external::python_engine::doctor_or_bail(profile, &python)?;
            print_json(&serde_json::json!({
                "engine": profile.engine_id,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        EngineCommands::Setup => {
            susi_agents::external::python_engine::bind_workspace_config(profile, workspace);
            let adapter = manager.adapter(profile.engine_id)?;
            print_json(&susi_agents::external::python_engine::setup_report(
                profile, &adapter,
            )?)?;
        }
        EngineCommands::Init => print_json(&susi_agents::external::python_engine::init_workspace(
            profile, workspace,
        )?)?,
        EngineCommands::Run { wait, prompt } => {
            susi_agents::external::python_engine::bind_workspace_config(profile, workspace);
            susi_agents::external::python_engine::doctor_or_bail(profile, &python)?;
            let run = manager.prepare(profile.engine_id, &prompt)?;
            launch(profile, &manager, &run.id, wait)?;
        }
        EngineCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == profile.engine_id)
                .collect();
            print_json(&runs)?;
        }
        EngineCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        EngineCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        EngineCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
    }
    Ok(())
}

fn launch(profile: &EngineProfile, manager: &AgentManager, id: &str, wait: bool) -> Result<()> {
    print_json(&manager.read(id)?)?;
    if wait {
        return run_worker(profile, manager.clone(), id);
    }
    // `/proc/self/exe` re-executes the live inode; `current_exe` reports a
    // `… (deleted)` path after an in-place binary replacement (ENOENT).
    let exe = susi_daemon::supervisor::reexec_path()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)?;
    let mut command = Command::new(exe);
    command
        .args(["frameworks", "worker", id])
        .current_dir(&manager.read(id)?.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    susi_agents::external::python_engine::ensure_process_banner(profile, &mut command);
    if let Ok(v) = std::env::var(profile.config_env) {
        command.env(profile.config_env, v);
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

fn run_worker(profile: &EngineProfile, manager: AgentManager, id: &str) -> Result<()> {
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
            "{} task {} ended with {:?}; inspect status and logs",
            profile.engine_id,
            run.id,
            run.status
        );
    }
    Ok(())
}
