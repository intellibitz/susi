//! `susi harness`: author a plan, run a mission, read the cockpit — the
//! orchestration surfaces driven from the susi binary itself.
//!
//! The gateway stays the meter. These commands call the same library the
//! `harness-*` binaries do, so the mission loop lives inside susi rather than
//! beside it.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use susi_harness::adapter::SusiTarget;
use susi_harness::author::{author_plan, AuthorConfig, AuthorRequest};
use susi_harness::balance::Budgets;
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::cockpit::{serve, Cockpit};
use susi_harness::executor::NativeWorker;
use susi_harness::fleet::{self, gather_budgets, measured_latency, total_spent, Fleet};
use susi_harness::money::MicroUsd;
use susi_harness::policy::Policy;
use susi_harness::providers::{Registry, Wire};
use susi_harness::queue;
use susi_harness::supervisor::{Plan, Supervisor, SusiExecutor};
use susi_harness::validate::{validate_plan, RepoFacts};
use susi_harness::verify::{fetch_http, verify_registry};

#[allow(clippy::large_enum_variant)]
// a CLI flag bag: every variant is argument storage, and boxing one would only relocate the flags clap derives in place
#[derive(Debug, Subcommand)]
pub enum HarnessCommands {
    /// Author a plan from a roadmap vector, through the metered gateway
    Author {
        /// Gateway base URL (the harness gateway)
        #[arg(long)]
        gateway: String,
        /// Model id the gateway prices (such as `deepseek-pro`)
        #[arg(long, default_value = "auto")]
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
        #[arg(long, default_value = "auto")]
        workers: String,
    },
    /// Plan a batch of dependency-ready queue tasks; each entry claims its id
    #[command(name = "plan")]
    PlanQueue {
        /// The susi target repository
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Where to write the plan
        #[arg(long, default_value = "plan.json")]
        out: PathBuf,
        /// Mission id
        #[arg(long, default_value = "roadmap-batch")]
        mission: String,
        /// Upper bound on tasks in the batch
        #[arg(long, default_value_t = 1)]
        limit: usize,
        /// Worker models, assigned round-robin (comma-separated)
        #[arg(long, default_value = "auto")]
        workers: String,
        /// Only tasks whose roadmap vector is this id
        #[arg(long)]
        roadmap: Option<String>,
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
        #[arg(long, default_value = "auto")]
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
        #[arg(long, default_value = "auto")]
        workers: String,
        /// Mission cap in dollars
        #[arg(long, default_value = "5")]
        budget_usd: String,
        /// Price catalogue used to rank workers and bound spend
        #[arg(long, default_value = "deploy/harness/prices.json")]
        catalogue: PathBuf,
        /// Daily spend ceiling in dollars, checked against the ledger before a run
        #[arg(long, default_value = "25")]
        daily_usd: String,
        /// Ledger the gateway writes (spend authority)
        #[arg(long, default_value = "out/ledger.jsonl")]
        ledger: PathBuf,
        /// Control log the cockpit writes pause/resume/abort to
        #[arg(long, default_value = "out/control.jsonl")]
        controls: PathBuf,
        /// Self-build lifecycle ledger the RSI scorecard reads
        #[arg(long, default_value = ".agents/rsi-loop.jsonl")]
        rsi_log: PathBuf,
        /// Hard cap on attempts per task
        #[arg(long, default_value_t = 3)]
        attempts: usize,
        /// Where the adapter keeps prompts and results
        #[arg(long, default_value = "harness-state")]
        adapter_state: PathBuf,
        /// The susi binary the adapter drives (default: this one)
        #[arg(long)]
        susi: Option<PathBuf>,
    },
    /// Print the self-build (RSI) scorecard for the mission lifecycle ledger
    Scorecard {
        /// Lifecycle ledger the supervisor writes
        #[arg(long, default_value = ".agents/rsi-loop.jsonl")]
        events: PathBuf,
        /// Limit the report to one task (repeatable)
        #[arg(long)]
        task: Vec<String>,
        /// Machine-readable report
        #[arg(long)]
        json: bool,
        /// Fail unless every task reached every phase
        #[arg(long)]
        require_complete: bool,
        /// Python executable
        #[arg(long, default_value = "python")]
        python: PathBuf,
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
        /// Agent-graph bus file (overrides SUSI_HARNESS_GRAPH and the
        /// graph.jsonl beside the ledger)
        #[arg(long)]
        graph: Option<PathBuf>,
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
    /// Print the cockpit snapshot for a ledger
    Status {
        /// Ledger the gateway wrote
        #[arg(long)]
        ledger: PathBuf,
        /// Event feed (optional)
        #[arg(long)]
        events: Option<PathBuf>,
    },
    /// Print the provider registry, models and aliases as JSON (no network)
    Models {
        /// Check each selected provider's live models endpoint for drift
        #[arg(long)]
        verify: bool,
        /// Limit the check to one provider id (requires --verify)
        #[arg(long, requires = "verify")]
        provider: Option<String>,
        /// Filter for models with valid API keys present and sort by budget cost
        #[arg(long)]
        available: bool,
    },
    /// Print the one-line budget report and any top-up alert, without running
    Budget {
        /// Ledger the spend is read from
        #[arg(long, default_value = "out/ledger.jsonl")]
        ledger: PathBuf,
    },
    /// Serve the metered multi-provider gateway directly from susi
    Gateway {
        /// Port to bind (default: 9180)
        #[arg(long, default_value_t = 9180)]
        port: u16,
        /// Interface to bind (default: 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Price catalogue
        #[arg(long, default_value = "deploy/harness/prices.json")]
        catalogue: PathBuf,
        /// Ledger path
        #[arg(long, default_value = "out/ledger.jsonl")]
        ledger: PathBuf,
        /// Upstream URL or "direct" for multi-provider routing
        #[arg(long, default_value = "direct")]
        upstream: String,
        /// Upstream call timeout in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Accounts file for authenticated ingress (optional)
        #[arg(long)]
        accounts: Option<PathBuf>,
    },
}

/// Run one `susi harness` command.
///
/// # Errors
///
/// Returns the first error from authoring, validation, execution or I/O.
pub fn execute(action: HarnessCommands, cwd: &Path) -> Result<()> {
    // Fail closed: an overlay the operator named but the harness cannot use
    // would otherwise run every command on a registry they did not ask for.
    if let Some(error) = Registry::overlay_error() {
        bail!("{error}");
    }
    match action {
        HarnessCommands::Budget { ledger } => budget_report(&ledger, cwd),
        HarnessCommands::Models {
            verify,
            provider,
            available,
        } => {
            if verify {
                models_verify(provider.as_deref())
            } else if available {
                models_available(cwd)
            } else {
                println!("{}", serde_json::to_string_pretty(&registry_json())?);
                Ok(())
            }
        }
        HarnessCommands::Gateway {
            port,
            host,
            catalogue,
            ledger,
            upstream,
            timeout,
            accounts,
        } => serve_gateway(
            port, &host, catalogue, ledger, upstream, timeout, accounts, cwd,
        ),
        HarnessCommands::Status { ledger, events } => {
            let cockpit = Cockpit {
                ledger_path: ledger,
                events_path: events.unwrap_or_default(),
                state_path: None,
                plan_path: None,
                controls_path: None,
                catalogue_path: None,
                graph_path: None,
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
            graph,
            budget_usd,
            mission,
            host,
            port,
        } => serve_cockpit(
            ledger, events, state, plan, controls, catalogue, graph, budget_usd, mission, &host,
            port,
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
        HarnessCommands::PlanQueue {
            repo,
            out,
            mission,
            limit,
            workers,
            roadmap,
        } => plan_queue(
            &repo,
            &out,
            &mission,
            limit,
            &workers,
            roadmap.as_deref(),
            cwd,
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
            daily_usd,
            ledger,
            controls,
            rsi_log,
            attempts,
            adapter_state,
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
            &budget_usd,
            catalogue,
            &daily_usd,
            &ledger,
            &controls,
            &rsi_log,
            attempts,
            &adapter_state,
            susi,
            cwd,
        ),
        HarnessCommands::Scorecard {
            events,
            task,
            json,
            require_complete,
            python,
        } => scorecard(&events, &task, json, require_complete, &python, cwd),
    }
}

/// Write a plan over dependency-ready queue tasks.
#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
/// The task ids a live claim already reserves, read from the shared remote.
/// A planner that ignores them spends a mission discovering the overlap.
fn claimed_ids(repo: &Path) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .args(["ls-remote", "--refs", "origin", "refs/claims/*"])
        .current_dir(repo)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|name| name.strip_prefix("refs/claims/"))
        .map(str::to_string)
        .collect()
}

#[allow(clippy::too_many_arguments)] // a command's flags, one per option
fn plan_queue(
    repo: &Path,
    out: &Path,
    mission: &str,
    limit: usize,
    workers: &str,
    roadmap: Option<&str>,
    cwd: &Path,
) -> Result<()> {
    let repo = if repo == Path::new(".") {
        cwd.to_path_buf()
    } else {
        repo.to_path_buf()
    };
    let (budgets, _) = gather_budgets(&cwd.join("out/ledger.jsonl"));
    alert_topups(&budgets);
    let latency = measured_latency(&cwd.join("out/ledger.jsonl"));
    let workers = resolve_fleet(workers, || default_catalogue(cwd), &budgets, &latency)?.ladder;
    let claimed = claimed_ids(&repo);
    let plan = queue::plan_batch(&repo, mission, limit, &workers, roadmap, &claimed)?;
    std::fs::write(out, serde_json::to_string_pretty(&plan)?)
        .with_context(|| format!("write {}", out.display()))?;
    println!(
        "wrote {} — mission {} with {} task(s): {}",
        out.display(),
        plan.mission,
        plan.tasks.len(),
        plan.tasks
            .iter()
            .map(|task| format!("{} -> {}", task.key, task.task.as_deref().unwrap_or("new")))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// Run the repository's scorecard against the lifecycle ledger.
///
/// The script owns the report format; this command makes it reachable from the
/// binary, so a task's loop is measured by a caller instead of by a human who
/// remembers to run a script.
#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn scorecard(
    events: &Path,
    tasks: &[String],
    json: bool,
    require_complete: bool,
    python: &Path,
    cwd: &Path,
) -> Result<()> {
    let mut command = std::process::Command::new(python);
    command
        .arg("scripts/check-rsi-scorecard.py")
        .arg("--events")
        .arg(events);
    for task in tasks {
        command.arg("--task").arg(task);
    }
    if json {
        command.arg("--json");
    }
    if require_complete {
        command.arg("--require-complete");
    }
    let status = command
        .current_dir(cwd)
        .status()
        .with_context(|| format!("run {} scripts/check-rsi-scorecard.py", python.display()))?;
    if !status.success() {
        bail!("scorecard exited with {status}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // a command's flags, not a reusable API
fn serve_cockpit(
    ledger: PathBuf,
    events: Option<PathBuf>,
    state: Option<PathBuf>,
    plan: Option<PathBuf>,
    controls: Option<PathBuf>,
    catalogue: Option<PathBuf>,
    graph: Option<PathBuf>,
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
        graph_path: graph,
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
    let (budgets, _) = gather_budgets(&cwd.join("out/ledger.jsonl"));
    alert_topups(&budgets);
    let request = AuthorRequest {
        vector: format!("{vector} {title}"),
        target: target.to_string(),
        packages: facts.packages.iter().cloned().collect(),
        workers: resolve_fleet(
            workers,
            || default_catalogue(cwd),
            &budgets,
            &measured_latency(&cwd.join("out/ledger.jsonl")),
        )?
        .ladder,
        narrative: tail(narrative, 4000),
        max_tasks,
        retry_hint: None,
    };
    let config = AuthorConfig {
        gateway_url: gateway.to_string(),
        model: model.to_string(),
        timeout_secs: 300,
    };
    let plan = author_plan(&config, &request).context("author the plan")?;
    let plan = match validate_plan(&plan, &facts) {
        Ok(()) => plan,
        Err(error) => {
            // One corrective retry: the model that authored the plan is the
            // cheapest fix for its own validation error, and the error text is
            // the only useful instruction it can get.
            eprintln!("the authored plan failed validation ({error}); retrying once");
            let mut retry = request;
            retry.retry_hint = Some(error.to_string());
            let retried = author_plan(&config, &retry).context("author the plan (retry)")?;
            validate_plan(&retried, &facts).context("validate the authored plan (retry)")?;
            retried
        }
    };
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

/// Run a plan under the budget gate.
///
/// Alerts on stderr before the first task when a keyed provider needs a
/// top-up, and prints the one-line budget report on stdout after the run,
/// whether or not the run succeeded.
#[allow(clippy::too_many_arguments)] // a command's flags, one per option
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
    budget_usd: &str,
    catalogue: PathBuf,
    daily_usd: &str,
    ledger: &Path,
    controls: &Path,
    rsi_log: &Path,
    attempts: usize,
    adapter_state: &Path,
    susi: Option<PathBuf>,
    cwd: &Path,
) -> Result<()> {
    let (before, spent_before) = gather_budgets(ledger);
    alert_topups(&before);
    let outcome = run_mission(
        plan_path,
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
        daily_usd,
        ledger,
        controls,
        rsi_log,
        attempts,
        adapter_state,
        susi,
        cwd,
        &before,
    );
    let (after, spent_after) = gather_budgets(ledger);
    let spend = total_spent(&spent_after).checked_sub(total_spent(&spent_before));
    let spend = spend.unwrap_or(MicroUsd::ZERO);
    println!("{}", after.report_line(spend, Registry::builtin()));
    outcome
}

/// `susi harness budget`: the report line and the top-up alert, read now.
fn budget_report(ledger: &Path, cwd: &Path) -> Result<()> {
    let path = if ledger.is_absolute() {
        ledger.to_path_buf()
    } else {
        cwd.join(ledger)
    };
    let (budgets, spent) = gather_budgets(&path);
    println!(
        "{}",
        budgets.report_line(total_spent(&spent), Registry::builtin())
    );
    alert_topups(&budgets);
    Ok(())
}

#[allow(clippy::too_many_arguments)] // a command's flags, one per option
fn run_mission(
    plan_path: &Path,
    repo: &Path,
    state: &Path,
    gateway: &str,
    model: &str,
    timeout: u64,
    parallel: usize,
    prefix: &str,
    workers: &str,
    budget_usd: &str,
    catalogue: PathBuf,
    daily_usd: &str,
    ledger: &Path,
    controls: &Path,
    rsi_log: &Path,
    attempts: usize,
    adapter_state: &Path,
    susi: Option<PathBuf>,
    cwd: &Path,
    budgets: &Budgets,
) -> Result<()> {
    let plan = Plan::load(plan_path).with_context(|| format!("load {}", plan_path.display()))?;
    // The default state is per mission, so two agents running missions in the
    // same directory never resume or overwrite each other's progress.
    let state_path = if state == Path::new("supervisor-state.json") {
        cwd.join(format!("supervisor-{}.json", plan.mission))
    } else {
        state.to_path_buf()
    };
    let repo = if repo == Path::new(".") {
        cwd.to_path_buf()
    } else {
        repo.to_path_buf()
    };
    let facts = RepoFacts::load(&repo).context("read the workspace's packages")?;
    validate_plan(&plan, &facts).context("validate the plan")?;
    let catalogue = PriceCatalogue::load(&catalogue).context("load the price catalogue")?;
    let budget = MicroUsd::parse_usd(budget_usd)
        .map(Budget::new)
        .with_context(|| format!("--budget-usd {budget_usd} is not a dollar amount"))?;
    let daily_cap = MicroUsd::parse_usd(daily_usd)
        .with_context(|| format!("--daily-usd {daily_usd} is not a dollar amount"))?;
    let spent_today = spent_today(ledger, now_unix_ms());
    if spent_today.micros() >= daily_cap.micros() {
        bail!(
            "--daily-usd {} already spent today ({}): refusing to start another run",
            daily_cap,
            spent_today
        );
    }
    // Every named worker stays on the ladder. The task's own model is moved to
    // the front by the adapter, so a task assigned a non-default worker still
    // has a stronger model to escalate to when it does not pass.
    let fleet = resolve_fleet(
        workers,
        || Ok(catalogue.clone()),
        budgets,
        &measured_latency(ledger),
    )?;
    let model = fleet::resolve_model(model, &fleet).map_err(|error| anyhow::anyhow!("{error}"))?;
    eprintln!(
        "fleet: orchestrator {model}; ladder {}",
        fleet.ladder.join(", ")
    );
    let alternates = fleet.ladder;
    let susi_program =
        susi.unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("susi")));
    let target = SusiTarget::new(repo, susi_program);
    let native = NativeWorker;
    let executor = SusiExecutor {
        target: &target,
        runner: &native,
        worktree_prefix: prefix.to_string(),
        gateway_url: gateway.to_string(),
        model: model.to_string(),
        alternates,
        catalogue: Some(catalogue),
        budget: Some(budget),
        ledger_path: ledger.to_path_buf(),
        mission: plan.mission.clone(),
        policy: Policy {
            max_attempts: attempts.max(1),
            ..Policy::default()
        },
        timeout: Duration::from_secs(timeout),
        state_dir: adapter_state.to_path_buf(),
        lifecycle_path: Some(rsi_log.to_path_buf()),
        guard: std::sync::OnceLock::new(),
        exhausted: AtomicBool::new(false),
    };
    let supervisor = Supervisor::new(&state_path, &executor)
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

/// Sum the ledger's priced spend for the UTC day containing `now_unix_ms`.
fn spent_today(ledger: &Path, now_unix_ms: u64) -> MicroUsd {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return MicroUsd::ZERO;
    };
    let day = now_unix_ms / 86_400_000;
    let total = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| {
            entry
                .get("started_unix_ms")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|started| started / 86_400_000 == day)
        })
        .filter_map(|entry| entry.get("cost").and_then(serde_json::Value::as_u64))
        .fold(0u64, u64::saturating_add);
    MicroUsd::from_micros(total)
}

/// Wall clock in milliseconds since the Unix epoch.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    text.get(text.len() - limit..).unwrap_or(text).to_string()
}

/// The fleet for `--workers` ([`fleet::resolve_fleet`]), with each model
/// left off named on stderr.
fn resolve_fleet(
    spec: &str,
    catalogue: impl FnOnce() -> susi_error::EaiResult<PriceCatalogue>,
    budgets: &Budgets,
    latency: &BTreeMap<String, u64>,
) -> Result<Fleet> {
    let fleet = fleet::resolve_fleet(spec, catalogue, budgets, latency)
        .map_err(|error| anyhow::anyhow!("--workers {spec}: {error}"))?;
    for note in &fleet.left_off {
        eprintln!("⚠ {note}");
    }
    Ok(fleet)
}

/// Print the top-up and expiry alerts on stderr.
fn alert_topups(budgets: &Budgets) {
    for line in fleet::alerts(budgets) {
        eprintln!("{line}");
    }
}

/// The price catalogue at the workspace's default location, or the one built
/// into the binary when the workspace has none.
fn default_catalogue(cwd: &Path) -> susi_error::EaiResult<PriceCatalogue> {
    PriceCatalogue::load_or_builtin(&cwd.join("deploy/harness/prices.json"))
}

/// Verify the registry's backend ids against the providers' live model lists.
///
/// Prints the report even when it fails, then exits non-zero so a caller can
/// gate on drift without parsing the prose.
fn models_verify(provider: Option<&str>) -> Result<()> {
    let report = verify_registry(Registry::builtin(), provider, &fetch_http)
        .context("verify the provider registry")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.ok {
        return Ok(());
    }
    let failed = report
        .providers
        .iter()
        .filter(|entry| !entry.ok)
        .map(|entry| entry.provider.as_str())
        .collect::<Vec<_>>();
    bail!("model verification failed for: {}", failed.join(", "));
}

fn models_available(cwd: &Path) -> Result<()> {
    let catalogue = default_catalogue(cwd).ok();
    let ledger = cwd.join("out/ledger.jsonl");
    let (budgets, _) = gather_budgets(&ledger);
    let report = fleet::available_report(catalogue.as_ref(), &budgets, &measured_latency(&ledger));
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn serve_gateway(
    port: u16,
    host: &str,
    catalogue_path: PathBuf,
    ledger_path: PathBuf,
    upstream: String,
    timeout: u64,
    accounts_path: Option<PathBuf>,
    cwd: &Path,
) -> Result<()> {
    let resolved_cat = if catalogue_path.is_absolute() {
        catalogue_path
    } else {
        cwd.join(catalogue_path)
    };
    let resolved_ledger = if ledger_path.is_absolute() {
        ledger_path
    } else {
        cwd.join(ledger_path)
    };
    if let Some(parent) = resolved_ledger.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let catalogue = PriceCatalogue::load_or_builtin(&resolved_cat)
        .with_context(|| format!("load price catalogue {}", resolved_cat.display()))?;

    let cache_dir = resolved_ledger.parent().map(Path::to_path_buf);
    let state = Arc::new(susi_harness::gateway::GatewayState {
        catalogue,
        ledger_path: resolved_ledger,
        upstream_chat_url: upstream,
        provider: "susi".into(),
        api_key: None,
        timeout_secs: timeout,
        max_output_tokens: None,
        cache: Some(Arc::new(susi_harness::cache::ResponseCache::new(cache_dir))),
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for harness gateway")?;

    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((host, port))
            .await
            .with_context(|| format!("bind harness gateway to {host}:{port}"))?;
        let addr = listener.local_addr()?;
        eprintln!("susi harness gateway listening on http://{addr}/v1 (full cache on)");

        if let Some(accounts_file) = accounts_path {
            let acc_resolved = if accounts_file.is_absolute() {
                accounts_file
            } else {
                cwd.join(accounts_file)
            };
            let text = std::fs::read_to_string(&acc_resolved)
                .with_context(|| format!("read accounts file {}", acc_resolved.display()))?;
            let ingress =
                susi_harness::ingress::Ingress::parse(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
            susi_harness::ingress::serve(listener, state, Arc::new(ingress))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        } else {
            susi_harness::gateway::serve(listener, state)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
    })
}

/// The provider registry as JSON: each provider with its models, a flat model
/// list a picker can filter, and the aliases that resolve to model ids.
fn registry_json() -> serde_json::Value {
    let registry = Registry::builtin();
    let mut providers = Vec::new();
    let mut models = Vec::new();
    for provider in registry.providers() {
        let wire = match provider.wire {
            Wire::OpenAi => "openai",
            Wire::Anthropic => "anthropic",
        };
        let mut records = Vec::new();
        for model in &provider.models {
            let record = serde_json::json!({
                "id": model.id,
                "provider": provider.id,
                "backend": model.backend,
                "context": model.capabilities.context,
                "tools": model.capabilities.tools,
                "streaming": model.capabilities.streaming,
                "vision": model.capabilities.vision,
            });
            records.push(record.clone());
            models.push(record);
        }
        providers.push(serde_json::json!({
            "id": provider.id,
            "wire": wire,
            "endpoint": provider.endpoint,
            "key_env": provider.key_envs,
            "models": records,
        }));
    }
    serde_json::json!({
        "providers": providers,
        "models": models,
        "aliases": registry.aliases(),
    })
}

#[cfg(test)]
mod tests {
    use super::HarnessCommands;
    use crate::cli::defs::{Cli, Commands};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn cockpit_parses_the_graph_bus() {
        let cli = Cli::try_parse_from([
            "susi",
            "harness",
            "cockpit",
            "--ledger",
            "out/ledger.jsonl",
            "--graph",
            "out/bus.jsonl",
        ])
        .unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Cockpit { graph, .. } => {
                assert_eq!(graph, Some(PathBuf::from("out/bus.jsonl")));
            }
            _ => panic!("expected harness cockpit"),
        }
    }

    #[test]
    fn models_parses_without_a_gateway() {
        let cli = Cli::try_parse_from(["susi", "harness", "models"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models {
                verify,
                provider,
                available,
            } => {
                assert!(!verify);
                assert!(!available);
                assert!(provider.is_none());
            }
            _ => panic!("expected harness models"),
        }
    }

    #[test]
    fn models_available_parses() {
        let cli = Cli::try_parse_from(["susi", "harness", "models", "--available"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models { available, .. } => {
                assert!(available);
            }
            _ => panic!("expected harness models --available"),
        }
    }

    #[test]
    fn gateway_parses_with_defaults() {
        let cli = Cli::try_parse_from(["susi", "harness", "gateway"]).unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Gateway {
                port,
                host,
                upstream,
                ..
            } => {
                assert_eq!(port, 9180);
                assert_eq!(host, "127.0.0.1");
                assert_eq!(upstream, "direct");
            }
            _ => panic!("expected harness gateway"),
        }
    }

    #[test]
    fn models_verify_parses_with_a_provider() {
        let cli = Cli::try_parse_from([
            "susi",
            "harness",
            "models",
            "--verify",
            "--provider",
            "gemini",
        ])
        .unwrap();
        let Some(Commands::Harness { action }) = cli.command else {
            panic!("expected the harness command");
        };
        match action {
            HarnessCommands::Models {
                verify, provider, ..
            } => {
                assert!(verify);
                assert_eq!(provider.as_deref(), Some("gemini"));
            }
            _ => panic!("expected harness models --verify"),
        }
    }

    #[test]
    fn models_provider_requires_verify() {
        assert!(
            Cli::try_parse_from(["susi", "harness", "models", "--provider", "gemini"]).is_err()
        );
    }
    #[test]
    fn daily_cap_sums_only_todays_priced_entries() {
        let dir = std::env::temp_dir().join(format!("susi-daily-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let ledger = dir.join("ledger.jsonl");
        let now = 1_792_950_000_000u64;
        let yesterday = now - 86_400_000;
        let body = format!(
            "{{\"started_unix_ms\":{now},\"cost\":3000000}}\n{{\"started_unix_ms\":{yesterday},\"cost\":10000000}}\n{{\"started_unix_ms\":{now},\"cost\":0,\"cached\":true}}\n"
        );
        std::fs::write(&ledger, body).expect("ledger");
        assert_eq!(super::spent_today(&ledger, now).micros(), 3_000_000);
        assert_eq!(
            super::spent_today(&dir.join("missing.jsonl"), now).micros(),
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
