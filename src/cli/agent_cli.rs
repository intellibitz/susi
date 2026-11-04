//! Deterministic external-agent control plane, available without inference/daemon boot.
use super::catalog_plane_cli::{launch, run_worker, LaunchOpts};
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use susi_agents::external::{catalog, AgentManager, CatalogKind};
use susi_daemon::adapter_ops::{self, AdapterKind};

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
        // `susi_daemon::adapter_ops` builds the doctor/setup/configure/reset
        // documents and checks retries; the MCP `adapter_*` and
        // `*_retry` tools call the same functions.
        AgentCommands::Setup { agent } => print_json(
            &adapter_ops::setup(AdapterKind::Agents, &agent, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        AgentCommands::Doctor { agent } => {
            let rows = adapter_ops::doctor(AdapterKind::Agents, agent.as_deref(), workspace)
                .map_err(anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!("{}", adapter_ops::needs_setup_message(AdapterKind::Agents));
            }
        }
        AgentCommands::Configure { agent, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Agents, &agent, doc, workspace)
                    .map_err(anyhow::Error::msg)?,
            )?;
        }
        AgentCommands::Reset { agent } => print_json(
            &adapter_ops::reset(AdapterKind::Agents, &agent, workspace)
                .map_err(anyhow::Error::msg)?,
        )?,
        AgentCommands::Run {
            agent,
            wait,
            prompt,
        } => {
            let run = manager.prepare(&agent, &prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["agents", "worker", &run.id],
                    banner: None,
                    fail_label: "agent",
                },
            )?;
        }
        AgentCommands::Retry { task_id, wait } => {
            let old = adapter_ops::retry_source(&manager, &task_id).map_err(anyhow::Error::msg)?;
            let run = manager.prepare(&old.agent, &old.prompt)?;
            launch(
                &manager,
                &run.id,
                LaunchOpts {
                    wait,
                    worker_argv: &["agents", "worker", &run.id],
                    banner: None,
                    fail_label: "agent",
                },
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
