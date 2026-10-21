//! `susi harness`: author a plan, run a mission, read the cockpit — the
//! orchestration surfaces driven from the susi binary itself.
//!
//! The gateway stays the meter. These commands call the same library the
//! `harness-*` binaries do, so the mission loop lives inside susi rather than
//! beside it.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::time::Duration;
use susi_harness::adapter::SusiTarget;
use susi_harness::author::{author_plan, AuthorConfig, AuthorRequest};
use susi_harness::cockpit::Cockpit;
use susi_harness::executor::OpenClaw;
use susi_harness::litellm::Launcher;
use susi_harness::supervisor::{Plan, Supervisor, SusiExecutor};
use susi_harness::validate::{validate_plan, RepoFacts};

#[derive(Debug, Subcommand)]
pub enum HarnessCommands {
    /// Author a plan from a roadmap vector, through the metered gateway
    Author {
        /// Gateway base URL (the harness gateway)
        #[arg(long)]
        gateway: String,
        /// Model id the gateway prices (a LiteLLM name such as `deepseek-chat`)
        #[arg(long)]
        model: String,
        /// Roadmap file
        #[arg(long, default_value = ".agents/roadmap.json")]
        roadmap: PathBuf,
        /// Vector id, e.g. VC-203-001
        vector: String,
        /// Where to write the plan
        #[arg(long, default_value = "plan.json")]
        out: PathBuf,
        /// Upper bound on tasks
        #[arg(long, default_value_t = 3)]
        max_tasks: usize,
    },
    /// Run a plan: prepare in parallel, land in order, persist after each task
    Run {
        /// Plan file
        plan: PathBuf,
        /// The susi target repository
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Supervisor resume state
        #[arg(long, default_value = "supervisor-state.json")]
        state: PathBuf,
        /// Gateway base URL (the harness gateway)
        #[arg(long)]
        gateway: String,
        /// Model id the gateway prices
        #[arg(long)]
        model: String,
        /// Per-turn deadline, seconds
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// Tasks prepared at once (landing is always serial)
        #[arg(long, default_value_t = 1)]
        parallel: usize,
        /// Worktree name prefix
        #[arg(long, default_value = "harness")]
        prefix: String,
        /// Where the adapter keeps prompts and results
        #[arg(long, default_value = "harness-state")]
        adapter_state: PathBuf,
        /// The agent executor (OpenClaw)
        #[arg(long, default_value = "openclaw")]
        program: PathBuf,
        /// The susi binary the adapter drives (default: this one)
        #[arg(long)]
        susi: Option<PathBuf>,
    },
    /// Start the LiteLLM proxy the gateway points at (caching + providers)
    Litellm {
        /// LiteLLM config file
        #[arg(long, default_value = "deploy/litellm/config.yaml")]
        config: PathBuf,
        /// Interface to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind
        #[arg(long, default_value_t = 4000)]
        port: u16,
        /// The litellm executable
        #[arg(long, default_value = "litellm")]
        litellm: PathBuf,
        /// Where the proxy log is written
        #[arg(long, default_value = "harness-litellm")]
        state: PathBuf,
        /// How long to wait for liveness, seconds
        #[arg(long, default_value_t = 120)]
        ready_timeout: u64,
    },
    /// Print the cockpit snapshot for a ledger
    Status {
        /// Ledger the gateway wrote
        #[arg(long)]
        ledger: PathBuf,
        /// Event feed (optional)
        #[arg(long)]
        events: Option<PathBuf>,
    },
}

/// Run one `susi harness` command.
///
/// # Errors
///
/// Returns the first error from authoring, validation, execution or I/O.
pub fn execute(action: HarnessCommands, cwd: &Path) -> Result<()> {
    match action {
        HarnessCommands::Litellm {
            config,
            host,
            port,
            litellm,
            state,
            ready_timeout,
        } => start_proxy(config, host, port, litellm, state, ready_timeout),
        HarnessCommands::Status { ledger, events } => {
            let cockpit = Cockpit {
                ledger_path: ledger,
                events_path: events.unwrap_or_default(),
            };
            println!("{}", serde_json::to_string_pretty(&cockpit.snapshot())?);
            Ok(())
        }
        HarnessCommands::Author {
            gateway,
            model,
            roadmap,
            vector,
            out,
            max_tasks,
        } => author(&gateway, &model, &roadmap, &vector, &out, max_tasks, cwd),
        HarnessCommands::Run {
            plan,
            repo,
            state,
            gateway,
            model,
            timeout,
            parallel,
            prefix,
            adapter_state,
            program,
            susi,
        } => run(
            &plan,
            &repo,
            &state,
            &gateway,
            &model,
            timeout,
            parallel,
            &prefix,
            &adapter_state,
            &program,
            susi,
            cwd,
        ),
    }
}

#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn start_proxy(
    config: PathBuf,
    host: String,
    port: u16,
    program: PathBuf,
    state: PathBuf,
    ready_timeout: u64,
) -> Result<()> {
    let launcher = Launcher {
        config,
        host: host.clone(),
        port,
        program,
        state_dir: state,
        ready_timeout: Duration::from_secs(ready_timeout),
    };
    for name in launcher.missing_keys()? {
        eprintln!("susi harness litellm: {name} is unset; models that use it fail until it is set");
    }
    let mut child = launcher.launch()?;
    println!("litellm listening on http://{host}:{port}");
    let status = child.wait().context("wait for litellm")?;
    if !status.success() {
        bail!("litellm exited with {status}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn author(
    gateway: &str,
    model: &str,
    roadmap: &Path,
    vector: &str,
    out: &Path,
    max_tasks: usize,
    cwd: &Path,
) -> Result<()> {
    let text = std::fs::read_to_string(roadmap)
        .with_context(|| format!("read roadmap {}", roadmap.display()))?;
    let document: serde_json::Value = serde_json::from_str(&text).context("parse roadmap")?;
    let entry = document
        .get("vectors")
        .and_then(serde_json::Value::as_array)
        .and_then(|vectors| {
            vectors.iter().find(|candidate| {
                candidate.get("id").and_then(serde_json::Value::as_str) == Some(vector)
            })
        })
        .with_context(|| format!("vector {vector} not found in {}", roadmap.display()))?;
    let title = entry
        .get("vector")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let target = entry
        .get("mastery_target")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let narrative = entry
        .get("progress")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let facts = RepoFacts::load(cwd).context("read the workspace's packages")?;
    let request = AuthorRequest {
        vector: format!("{vector} {title}"),
        target: target.to_string(),
        packages: facts.packages.iter().cloned().collect(),
        narrative: tail(narrative, 4000),
        max_tasks,
    };
    let config = AuthorConfig {
        gateway_url: gateway.to_string(),
        model: model.to_string(),
        timeout_secs: 300,
    };
    let plan = author_plan(&config, &request).context("author the plan")?;
    validate_plan(&plan, &facts).context("validate the authored plan")?;
    std::fs::write(out, serde_json::to_string_pretty(&plan)?)
        .with_context(|| format!("write {}", out.display()))?;
    println!(
        "wrote {} — mission {} with {} task(s): {}",
        out.display(),
        plan.mission,
        plan.tasks.len(),
        plan.tasks
            .iter()
            .map(|task| task.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run(
    plan_path: &Path,
    repo: &Path,
    state: &Path,
    gateway: &str,
    model: &str,
    timeout: u64,
    parallel: usize,
    prefix: &str,
    adapter_state: &Path,
    program: &Path,
    susi: Option<PathBuf>,
    cwd: &Path,
) -> Result<()> {
    let plan = Plan::load(plan_path).with_context(|| format!("load {}", plan_path.display()))?;
    let repo = if repo == Path::new(".") {
        cwd.to_path_buf()
    } else {
        repo.to_path_buf()
    };
    let facts = RepoFacts::load(&repo).context("read the workspace's packages")?;
    validate_plan(&plan, &facts).context("validate the plan")?;
    let susi_program =
        susi.unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("susi")));
    let target = SusiTarget::new(repo, susi_program);
    let openclaw = OpenClaw;
    let executor = SusiExecutor {
        target: &target,
        runner: &openclaw,
        worktree_prefix: prefix.to_string(),
        program: program.to_path_buf(),
        gateway_url: gateway.to_string(),
        model: model.to_string(),
        timeout: Duration::from_secs(timeout),
        state_dir: adapter_state.to_path_buf(),
    };
    let supervisor = Supervisor::new(state, &executor).with_parallelism(parallel);
    let report = supervisor.run(&plan).context("run the mission")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.failed.is_empty() {
        bail!(
            "mission {} failed {} task(s)",
            report.mission,
            report.failed.len()
        );
    }
    Ok(())
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    text.get(text.len() - limit..).unwrap_or(text).to_string()
}
