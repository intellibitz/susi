//! `susi harness`: author a plan, run a mission, read the cockpit — the
//! orchestration surfaces driven from the susi binary itself.
//!
//! The gateway stays the meter. These commands call the same library the
//! `harness-*` binaries do, so the mission loop lives inside susi rather than
//! beside it.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use susi_harness::adapter::SusiTarget;
use susi_harness::author::{author_plan, AuthorConfig, AuthorRequest};
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::cockpit::{serve, Cockpit};
use susi_harness::executor::OpenClaw;
use susi_harness::litellm::Launcher;
use susi_harness::money::MicroUsd;
use susi_harness::policy::Policy;
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
        /// Worker models tasks are spread across (comma-separated)
        #[arg(long, default_value = "deepseek-chat,gemini-flash")]
        workers: String,
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
        /// Models a task may escalate to when its own model does not pass
        /// (comma-separated)
        #[arg(long, default_value = "")]
        workers: String,
        /// Mission cap in dollars; requires --catalogue
        #[arg(long)]
        budget_usd: Option<String>,
        /// Price catalogue used to rank workers and bound spend
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Ledger the gateway writes (spend authority)
        #[arg(long, default_value = "out/ledger.jsonl")]
        ledger: PathBuf,
        /// Control log the cockpit writes pause/resume/abort to
        #[arg(long, default_value = "out/control.jsonl")]
        controls: PathBuf,
        /// Hard cap on attempts per task
        #[arg(long, default_value_t = 4)]
        attempts: usize,
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
    /// Serve the CEO cockpit: spend, tasks, workers, warnings and controls
    Cockpit {
        /// Ledger the gateway writes
        #[arg(long)]
        ledger: PathBuf,
        /// Event feed (default: beside the ledger)
        #[arg(long)]
        events: Option<PathBuf>,
        /// Supervisor resume state to show task progress
        #[arg(long)]
        state: Option<PathBuf>,
        /// Plan the supervisor is running
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Control log the operator's commands are appended to
        #[arg(long)]
        controls: Option<PathBuf>,
        /// Price catalogue, so each worker's rate is shown
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Mission cap in dollars
        #[arg(long)]
        budget_usd: Option<String>,
        /// Mission to focus
        #[arg(long)]
        mission: Option<String>,
        /// Interface to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind
        #[arg(long, default_value_t = 9181)]
        port: u16,
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
                state_path: None,
                plan_path: None,
                controls_path: None,
                catalogue_path: None,
                budget: None,
                mission: None,
            };
            println!("{}", serde_json::to_string_pretty(&cockpit.snapshot())?);
            Ok(())
        }
        HarnessCommands::Cockpit {
            ledger,
            events,
            state,
            plan,
            controls,
            catalogue,
            budget_usd,
            mission,
            host,
            port,
        } => serve_cockpit(
            ledger, events, state, plan, controls, catalogue, budget_usd, mission, &host, port,
        ),
        HarnessCommands::Author {
            gateway,
            model,
            roadmap,
            vector,
            out,
            max_tasks,
            workers,
        } => author(
            &gateway, &model, &roadmap, &vector, &out, max_tasks, &workers, cwd,
        ),
        HarnessCommands::Run {
            plan,
            repo,
            state,
            gateway,
            model,
            timeout,
            parallel,
            prefix,
            workers,
            budget_usd,
            catalogue,
            ledger,
            controls,
            attempts,
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
            &workers,
            budget_usd.as_deref(),
            catalogue,
            &ledger,
            &controls,
            attempts,
            &adapter_state,
            &program,
            susi,
            cwd,
        ),
    }
}

#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn serve_cockpit(
    ledger: PathBuf,
    events: Option<PathBuf>,
    state: Option<PathBuf>,
    plan: Option<PathBuf>,
    controls: Option<PathBuf>,
    catalogue: Option<PathBuf>,
    budget_usd: Option<String>,
    mission: Option<String>,
    host: &str,
    port: u16,
) -> Result<()> {
    let events = events.unwrap_or_else(|| ledger.with_file_name("events.jsonl"));
    let budget = budget_usd
        .as_deref()
        .map(|amount| {
            MicroUsd::parse_usd(amount)
                .map(Budget::new)
                .with_context(|| format!("--budget-usd {amount} is not a dollar amount"))
        })
        .transpose()?;
    let cockpit = Arc::new(Cockpit {
        ledger_path: ledger,
        events_path: events,
        state_path: state,
        plan_path: plan,
        controls_path: controls,
        catalogue_path: catalogue,
        budget,
        mission,
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the cockpit runtime")?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((host, port))
            .await
            .with_context(|| format!("bind {host}:{port}"))?;
        let address = listener.local_addr().context("cockpit address")?;
        println!("susi cockpit listening on http://{address}");
        serve(listener, cockpit).await.context("serve the cockpit")
    })
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
    workers: &str,
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
        workers: workers
            .split(',')
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
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
    workers: &str,
    budget_usd: Option<&str>,
    catalogue: Option<PathBuf>,
    ledger: &Path,
    controls: &Path,
    attempts: usize,
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
    let catalogue = catalogue
        .map(|path| PriceCatalogue::load(&path))
        .transpose()
        .context("load the price catalogue")?;
    let budget = budget_usd
        .map(|amount| {
            MicroUsd::parse_usd(amount)
                .map(Budget::new)
                .with_context(|| format!("--budget-usd {amount} is not a dollar amount"))
        })
        .transpose()?;
    if budget.is_some() && catalogue.is_none() {
        bail!("--budget-usd needs --catalogue: an unpriced attempt cannot be bounded");
    }
    // Every named worker stays on the ladder. The task's own model is moved to
    // the front by the adapter, so a task assigned a non-default worker still
    // has a stronger model to escalate to when it does not pass.
    let alternates: Vec<String> = workers
        .split(',')
        .map(str::trim)
        .filter(|worker| !worker.is_empty())
        .map(str::to_string)
        .collect();
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
        alternates,
        catalogue,
        budget,
        ledger_path: ledger.to_path_buf(),
        mission: plan.mission.clone(),
        policy: Policy {
            max_attempts: attempts.max(1),
            ..Policy::default()
        },
        timeout: Duration::from_secs(timeout),
        state_dir: adapter_state.to_path_buf(),
        guard: std::sync::OnceLock::new(),
        exhausted: AtomicBool::new(false),
    };
    let supervisor = Supervisor::new(state, &executor)
        .with_parallelism(parallel)
        .with_controls(controls);
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
