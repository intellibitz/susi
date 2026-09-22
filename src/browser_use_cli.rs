//! Deterministic Browser Use control plane (no daemon required).
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::process::{Command, Stdio};
use susi_agents::external::{
    browser_use_doctor, browser_use_setup, browser_use_status, AgentManager, CatalogKind,
    RunStatus, BROWSER_USE_AGENT_ID,
};

#[derive(Debug, Subcommand)]
pub enum BrowserUseCommands {
    /// Show Browser Use binary + auth/browser status
    List,
    /// Check CLI binary and credentials (does not start a paid run)
    Doctor,
    /// Show install/auth instructions and effective adapter
    Setup,
    /// Start a headless Browser Use task (same lifecycle as `susi agents run browser-use`)
    Run {
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable Browser Use tasks in this workspace
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

pub fn execute(action: Option<BrowserUseCommands>, workspace: &Path) -> Result<()> {
    let action = action.unwrap_or(BrowserUseCommands::List);
    let manager = AgentManager::new(workspace)?;
    match action {
        BrowserUseCommands::List => {
            let def =
                susi_agents::external::definition(CatalogKind::Execution, BROWSER_USE_AGENT_ID)?;
            print_json(&serde_json::json!({
                "status": browser_use_status(),
                "definition": def,
            }))?;
        }
        BrowserUseCommands::Doctor => {
            let detail = browser_use_doctor()?;
            print_json(&serde_json::json!({
                "agent": BROWSER_USE_AGENT_ID,
                "prerequisites_present": true,
                "detail": detail,
            }))?;
        }
        BrowserUseCommands::Setup => print_json(&browser_use_setup())?,
        BrowserUseCommands::Run { wait, prompt } => {
            browser_use_doctor()?;
            let run = manager.prepare(BROWSER_USE_AGENT_ID, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        BrowserUseCommands::Tasks => {
            let runs: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|r| r.agent == BROWSER_USE_AGENT_ID)
                .collect();
            print_json(&runs)?;
        }
        BrowserUseCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        BrowserUseCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => {
            print!("{}", manager.logs(&task_id, stderr, bytes)?);
        }
        BrowserUseCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
        command.env("SUSI_PROCESS_BANNER", "susi-browser-use");
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
            "browser-use task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
