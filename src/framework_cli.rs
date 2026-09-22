//! Deterministic agent-framework control plane (LangGraph, CrewAI, …), no daemon required.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use susi_agents::external::{catalog, Adapter, AgentManager, CatalogKind, RunStatus};

#[derive(Debug, Subcommand)]
pub enum FrameworkCommands {
    /// Show the ten agent frameworks/engines and setup documentation
    List,
    /// Check local prerequisites (does not invoke a paid model)
    Doctor {
        engine: Option<String>,
    },
    /// Show setup instructions and the effective adapter configuration
    Setup {
        engine: String,
    },
    /// Load an adapter JSON file into trusted host configuration
    Configure {
        engine: String,
        file: PathBuf,
    },
    /// Remove a host override and restore the bundled adapter
    Reset {
        engine: String,
    },
    /// Start a task; defaults to a detached worker with durable output
    Run {
        engine: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable framework tasks in the current workspace
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
    Retry {
        task_id: String,
        #[arg(long)]
        wait: bool,
    },
    #[command(hide = true)]
    Worker {
        task_id: String,
    },
}

pub fn execute(action: FrameworkCommands, workspace: &Path) -> Result<()> {
    let manager = AgentManager::frameworks(workspace)?;
    match action {
        FrameworkCommands::List => print_json(&catalog(CatalogKind::Framework)?)?,
        FrameworkCommands::Setup { engine } => {
            let definition = susi_agents::external::definition(CatalogKind::Framework, &engine)?;
            print_json(&serde_json::json!({
                "engine": definition.id,
                "documentation": definition.documentation,
                "adapter": manager.adapter(&engine)?,
                "instructions": "Install the Python package (pip/uv). Set the adapter config_env to an absolute JSON file with either {\"script\":\"/abs/path.py\"} or {\"entrypoint\":\"module:callable\"}. The callable receives the prompt string. Model API keys stay in your environment. Run doctor, then run. No packages or credentials are provisioned implicitly."
            }))?;
        }
        FrameworkCommands::Doctor { engine } => {
            let engines = match engine {
                Some(id) => vec![susi_agents::external::definition(
                    CatalogKind::Framework,
                    &id,
                )?],
                None => catalog(CatalogKind::Framework)?,
            };
            let mut missing = false;
            for engine in engines {
                let result = manager.adapter(&engine.id).and_then(|a| a.preflight());
                missing |= result.is_err();
                print_json(&serde_json::json!({
                    "engine": engine.id,
                    "prerequisites_present": result.is_ok(),
                    "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                }))?;
            }
            if missing {
                bail!("one or more frameworks need setup");
            }
        }
        FrameworkCommands::Configure { engine, file } => {
            let adapter: Adapter = serde_json::from_slice(&std::fs::read(file)?)?;
            manager.configure(&engine, &adapter)?;
            print_json(&adapter)?;
        }
        FrameworkCommands::Reset { engine } => {
            manager.reset(&engine)?;
            print_json(&manager.adapter(&engine)?)?;
        }
        FrameworkCommands::Run {
            engine,
            wait,
            prompt,
        } => {
            let run = manager.prepare(&engine, &prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        FrameworkCommands::Retry { task_id, wait } => {
            let old = manager.status(&task_id)?;
            if old.status.active() || old.status == RunStatus::Unknown {
                bail!("resolve the existing task before retrying to avoid duplicate execution");
            }
            let run = manager.prepare(&old.agent, &old.prompt)?;
            launch(&manager, &run.id, wait)?;
        }
        FrameworkCommands::Worker { task_id } => run_worker(manager, &task_id)?,
        FrameworkCommands::Tasks => print_json(&manager.list()?)?,
        FrameworkCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        FrameworkCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => print!("{}", manager.logs(&task_id, stderr, bytes)?),
        FrameworkCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
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
            "framework task {} ended with {:?}; inspect status and logs",
            run.id,
            run.status
        );
    }
    Ok(())
}
