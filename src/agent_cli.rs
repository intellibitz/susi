//! Deterministic external-agent control plane, available without inference/daemon boot.
use crate::catalog_plane_cli::{launch, run_worker};
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use susi_agents::external::{catalog, Adapter, AgentManager, CatalogKind, RunStatus};

#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    /// Show curated executors, native adapters, and installation documentation
    List,
    /// Check local prerequisites (does not invoke a paid model)
    Doctor { agent: Option<String> },
    /// Show setup instructions and the effective adapter configuration
    Setup { agent: String },
    /// Load an adapter JSON file into trusted host configuration
    Configure { agent: String, file: PathBuf },
    /// Remove a host override and restore the bundled adapter
    Reset { agent: String },
    /// Start a task; defaults to a detached worker with durable output
    Run {
        agent: String,
        /// Wait for the task to finish in this process
        #[arg(long)]
        wait: bool,
        /// Task prompt, passed as a single argv value, never evaluated by a shell
        prompt: String,
    },
    /// List durable tasks in the current workspace
    Tasks,
    /// Inspect a task; --refresh queries a cloud provider after worker loss/completion
    Status {
        task_id: String,
        #[arg(long)]
        refresh: bool,
    },
    /// Read the end of the task's captured output
    Logs {
        task_id: String,
        #[arg(long)]
        stderr: bool,
        #[arg(long, default_value_t = 65536)]
        bytes: u64,
    },
    /// Request cancellation; cloud tasks are stopped through their native API
    Cancel { task_id: String },
    /// Send a follow-up to an existing cloud task; then use status --refresh
    Send { task_id: String, message: String },
    /// Run a new task with the original prompt (does not resume the vendor session)
    Retry {
        task_id: String,
        #[arg(long)]
        wait: bool,
    },
    #[command(hide = true)]
    Worker { task_id: String },
}

pub fn execute(action: AgentCommands, workspace: &Path) -> Result<()> {
    let manager = AgentManager::new(workspace)?;
    match action {
        AgentCommands::List => print_json(&catalog(CatalogKind::Execution)?)?,
        AgentCommands::Setup { agent } => {
            let definition = susi_agents::external::definition(CatalogKind::Execution, &agent)?;
            print_json(&serde_json::json!({
                "agent": definition.id,
                "documentation": definition.documentation,
                "adapter": manager.adapter(&agent)?,
                "instructions": "Install/authenticate the vendor CLI or set the cloud credential environment variable. Qwen requires qwen-agent and SUSI_QWEN_CONFIG. Run doctor, then run. No packages, subscriptions or credentials are provisioned implicitly."
            }))?;
        }
        AgentCommands::Doctor { agent } => {
            let agents = match agent {
                Some(id) => vec![susi_agents::external::definition(
                    CatalogKind::Execution,
                    &id,
                )?],
                None => catalog(CatalogKind::Execution)?,
            };
            let mut missing = false;
            for agent in agents {
                let result = manager.adapter(&agent.id).and_then(|a| a.preflight());
                missing |= result.is_err();
                print_json(
                    &serde_json::json!({"agent":agent.id, "prerequisites_present":result.is_ok(),
                    "detail":match result { Ok(s) => s, Err(e) => e.to_string() }}),
                )?;
            }
            if missing {
                bail!("one or more agents need setup");
            }
        }
        AgentCommands::Configure { agent, file } => {
            let adapter: Adapter = serde_json::from_slice(&std::fs::read(file)?)?;
            manager.configure(&agent, &adapter)?;
            print_json(&adapter)?;
        }
        AgentCommands::Reset { agent } => {
            manager.reset(&agent)?;
            print_json(&manager.adapter(&agent)?)?;
        }
        AgentCommands::Run {
            agent,
            wait,
            prompt,
        } => {
            let run = manager.prepare(&agent, &prompt)?;
            launch(
                &manager,
                &run.id,
                wait,
                &["agents", "worker", &run.id],
                None,
                "agent",
            )?;
        }
        AgentCommands::Retry { task_id, wait } => {
            let old = manager.status(&task_id)?;
            if old.status.active() || old.status == RunStatus::Unknown {
                bail!("resolve the existing task before retrying to avoid duplicate execution");
            }
            let run = manager.prepare(&old.agent, &old.prompt)?;
            launch(
                &manager,
                &run.id,
                wait,
                &["agents", "worker", &run.id],
                None,
                "agent",
            )?;
        }
        AgentCommands::Worker { task_id } => run_worker(manager, &task_id, "agent")?,
        AgentCommands::Tasks => print_json(&manager.list()?)?,
        AgentCommands::Status { task_id, refresh } => {
            print_json(&if refresh {
                manager.refresh(&task_id)?
            } else {
                manager.status(&task_id)?
            })?;
        }
        AgentCommands::Logs {
            task_id,
            stderr,
            bytes,
        } => print!("{}", manager.logs(&task_id, stderr, bytes)?),
        AgentCommands::Cancel { task_id } => print_json(&manager.cancel(&task_id)?)?,
        AgentCommands::Send { task_id, message } => print_json(&manager.send(&task_id, &message)?)?,
    }
    Ok(())
}
