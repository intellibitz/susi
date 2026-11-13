//! The operator commands behind `susi harness plan|author|run|status|budget|
//! models`, as library calls: the CLI prints what they return, and the
//! daemon serves the same calls as the MCP `harness_*` tools (zero-UI plan
//! B4 group 1). Serving the harness API and the gateway stays with the CLI:
//! those are process entry points.
//!
//! Operator notes (top-up alerts, models left off the fleet, probe results)
//! go to a [`Notes`] sink as they happen: the CLI writes them to stderr, the
//! daemon collects them into the tool result.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::{json, Value};
use susi_error::{EaiError, EaiResult, ResultExt as _};

use crate::adapter::SusiTarget;
use crate::author::{author_plan, AuthorConfig, AuthorRequest};
use crate::balance::Budgets;
use crate::budget::Budget;
use crate::catalog::PriceCatalogue;
use crate::cockpit::Cockpit;
use crate::executor::NativeWorker;
use crate::fleet::{self, gather_budgets, total_spent, Fleet};
use crate::money::MicroUsd;
use crate::policy::Policy;
use crate::providers::{Registry, Wire};
use crate::queue;
use crate::supervisor::{Plan, Supervisor, SupervisorReport, SusiExecutor};
use crate::validate::{validate_plan, RepoFacts};
use crate::verify::{fetch_http, verify_registry};

/// Where operator notes go as they happen (stderr in the CLI).
pub type Notes<'a> = &'a mut dyn FnMut(String);

fn bail<T>(message: impl Into<String>) -> EaiResult<T> {
    Err(EaiError::internal(message.into()))
}

/// `path` against `cwd` when it is relative.
pub fn resolve(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// `susi harness status`: the harness API snapshot for a ledger (with the
/// supervisor's state and plan when given).
pub fn status(
    ledger: PathBuf,
    events: Option<PathBuf>,
    state: Option<PathBuf>,
    plan: Option<PathBuf>,
) -> EaiResult<Value> {
    let cockpit = Cockpit {
        ledger_path: ledger,
        events_path: events.unwrap_or_default(),
        state_path: state,
        plan_path: plan,
        controls_path: None,
        catalogue_path: None,
        graph_path: None,
        budget: None,
        mission: None,
    };
    Ok(serde_json::to_value(cockpit.snapshot())?)
}

/// `susi harness budget`: the one-line budget report, the ROI line against
/// the frontier baseline (`null` until the ledger records one); the top-up
/// and expiry alerts go to `notes` and into the document.
pub fn budget(ledger: &Path, cwd: &Path, notes: Notes<'_>) -> Value {
    let path = resolve(cwd, ledger);
    let (budgets, spent) = gather_budgets(&path);
    let report = budgets.report_line(total_spent(&spent), Registry::builtin());
    let (entries, _) = crate::ledger::read_entries_lenient(&path);
    let (roi, _) = crate::roi::RoiReport::from_entries(&entries);
    let roi = (roi.frontier_baseline_micro_usd > 0).then(|| roi.summary_line());
    let alerts = fleet::alerts(&budgets);
    for line in &alerts {
        notes(line.clone());
    }
    json!({ "ledger": path, "report": report, "roi": roi, "alerts": alerts })
}

/// What `susi harness models` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsMode {
    /// The provider registry, models and aliases (no network).
    Registry,
    /// Each selected provider's live models endpoint, checked for drift.
    Verify,
    /// The chat models keyed providers serve beyond the registry.
    Discover,
    /// Models with keys present, sorted by budget cost.
    Available,
}

/// `susi harness models`. `Verify` returns the report even when it fails;
/// [`verify_failures`] names the providers that did.
pub fn models(
    mode: ModelsMode,
    provider: Option<&str>,
    cwd: &Path,
    notes: Notes<'_>,
) -> EaiResult<Value> {
    match mode {
        ModelsMode::Registry => Ok(registry_json()),
        ModelsMode::Verify => {
            let report = verify_registry(Registry::builtin(), provider, &fetch_http)
                .context("verify the provider registry")?;
            Ok(serde_json::to_value(report)?)
        }
        ModelsMode::Discover => {
            let discovery = crate::verify::discover(Registry::builtin(), provider, &fetch_http)
                .context("discover the providers' models")?;
            for note in &discovery.notes {
                notes(note.clone());
            }
            Ok(serde_json::to_value(discovery)?)
        }
        ModelsMode::Available => {
            let catalogue = default_catalogue(cwd).ok();
            let ledger = fleet::default_ledger(cwd);
            let (budgets, _) = gather_budgets(&ledger);
            let evidence = live_evidence(&ledger, cwd, notes);
            Ok(serde_json::to_value(fleet::available_report(
                catalogue.as_ref(),
                &budgets,
                &evidence,
            ))?)
        }
    }
}

/// The providers a `Verify` report failed for (empty when it passed).
pub fn verify_failures(report: &Value) -> Vec<String> {
    if report.get("ok").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    report
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("ok").and_then(Value::as_bool) != Some(true))
        .filter_map(|entry| entry.get("provider").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The provider registry as JSON: each provider with its models, a flat model
/// list a picker can filter, and the aliases that resolve to model ids.
pub fn registry_json() -> Value {
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
            let record = json!({
                "id": model.id,
                "provider": provider.id,
                "backend": model.backend,
                "context": model.capabilities.context,
                "tools": model.capabilities.tools,
                "streaming": model.capabilities.streaming,
                "vision": model.capabilities.vision,
                "cache": model.capabilities.cache,
            });
            records.push(record.clone());
            models.push(record);
        }
        providers.push(json!({
            "id": provider.id,
            "wire": wire,
            "endpoint": provider.endpoint,
            "key_env": provider.key_envs,
            "models": records,
        }));
    }
    json!({
        "providers": providers,
        "models": models,
        "aliases": registry.aliases(),
    })
}

/// `susi harness plan` options.
#[derive(Debug, Clone)]
pub struct PlanQueueOptions {
    /// The susi target repository.
    pub repo: PathBuf,
    /// Where to write the plan.
    pub out: PathBuf,
    pub mission: String,
    /// Upper bound on tasks in the batch.
    pub limit: usize,
    /// Worker models, comma-separated, or `auto`.
    pub workers: String,
    /// Only tasks whose roadmap vector is this id.
    pub roadmap: Option<String>,
    /// Gateway the plan will run through.
    pub gateway: Option<String>,
}

/// `susi harness plan`: write a plan over dependency-ready queue tasks.
/// Returns `{plan, out, message}`; `message` is the line the CLI prints.
pub fn plan_queue(opts: &PlanQueueOptions, cwd: &Path, notes: Notes<'_>) -> EaiResult<Value> {
    let repo = if opts.repo == Path::new(".") {
        cwd.to_path_buf()
    } else {
        opts.repo.clone()
    };
    let (budgets, _) = gather_budgets(&fleet::default_ledger(cwd));
    for line in fleet::alerts(&budgets) {
        notes(line);
    }
    let evidence = live_evidence(&fleet::default_ledger(cwd), cwd, notes);
    // Tasks go to the light workers, spread across them; the fixers and the
    // orchestrator are not assigned first-line work.
    let workers = resolve_fleet(
        &opts.workers,
        || default_catalogue(cwd),
        &budgets,
        &evidence,
        opts.gateway.as_deref(),
        notes,
    )?
    .task_workers();
    let claimed = claimed_ids(&repo);
    let plan = queue::plan_batch(
        &repo,
        &opts.mission,
        opts.limit,
        &workers,
        opts.roadmap.as_deref(),
        &claimed,
    )?;
    std::fs::write(&opts.out, serde_json::to_string_pretty(&plan)?)
        .with_context(|| format!("write {}", opts.out.display()))?;
    let message = format!(
        "wrote {} — mission {} with {} task(s): {}",
        opts.out.display(),
        plan.mission,
        plan.tasks.len(),
        plan.tasks
            .iter()
            .map(|task| format!("{} -> {}", task.key, task.task.as_deref().unwrap_or("new")))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(json!({ "plan": plan, "out": opts.out, "message": message }))
}

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

/// `susi harness author` options.
#[derive(Debug, Clone)]
pub struct AuthorOptions {
    /// Gateway base URL (the harness gateway).
    pub gateway: String,
    /// Model id the gateway prices, or `auto`.
    pub model: String,
    /// Roadmap file.
    pub roadmap: PathBuf,
    /// Vector id, e.g. VC-203-001.
    pub vector: String,
    /// Where to write the plan.
    pub out: PathBuf,
    pub max_tasks: usize,
    /// Worker models, comma-separated, or `auto`.
    pub workers: String,
}

/// `susi harness author`: author a plan for a roadmap vector through the
/// metered gateway, validate it (one corrective retry) and write it.
/// Returns `{plan, out, message}`.
pub fn author(opts: &AuthorOptions, cwd: &Path, notes: Notes<'_>) -> EaiResult<Value> {
    let text = std::fs::read_to_string(&opts.roadmap)
        .with_context(|| format!("read roadmap {}", opts.roadmap.display()))?;
    let document: Value = serde_json::from_str(&text).context("parse roadmap")?;
    let vector = opts.vector.as_str();
    let Some(entry) = document
        .get("vectors")
        .and_then(Value::as_array)
        .and_then(|vectors| {
            vectors
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(vector))
        })
    else {
        return bail(format!(
            "vector {vector} not found in {}",
            opts.roadmap.display()
        ));
    };
    let text_of = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
    let facts = RepoFacts::load(cwd).context("read the workspace's packages")?;
    let (budgets, _) = gather_budgets(&fleet::default_ledger(cwd));
    for line in fleet::alerts(&budgets) {
        notes(line);
    }
    let evidence = live_evidence(&fleet::default_ledger(cwd), cwd, notes);
    let fleet = resolve_fleet(
        &opts.workers,
        || default_catalogue(cwd),
        &budgets,
        &evidence,
        Some(&opts.gateway),
        notes,
    )?;
    let request = AuthorRequest {
        vector: format!("{vector} {}", text_of("vector")),
        target: text_of("mastery_target").to_string(),
        packages: facts.packages.iter().cloned().collect(),
        workers: fleet.task_workers(),
        narrative: tail(text_of("progress"), 4000),
        max_tasks: opts.max_tasks,
        retry_hint: None,
    };
    // The orchestrator plans: `--model auto` is the fleet's strong model.
    let config = AuthorConfig {
        gateway_url: opts.gateway.clone(),
        model: fleet::resolve_model(&opts.model, &fleet)?,
        timeout_secs: 300,
    };
    let plan = author_plan(&config, &request).context("author the plan")?;
    let plan = match validate_plan(&plan, &facts) {
        Ok(()) => plan,
        Err(error) => {
            // One corrective retry: the model that authored the plan is the
            // cheapest fix for its own validation error, and the error text is
            // the only useful instruction it can get.
            notes(format!(
                "the authored plan failed validation ({error}); retrying once"
            ));
            let mut retry = request;
            retry.retry_hint = Some(error.to_string());
            let retried = author_plan(&config, &retry).context("author the plan (retry)")?;
            validate_plan(&retried, &facts).context("validate the authored plan (retry)")?;
            retried
        }
    };
    std::fs::write(&opts.out, serde_json::to_string_pretty(&plan)?)
        .with_context(|| format!("write {}", opts.out.display()))?;
    let message = format!(
        "wrote {} — mission {} with {} task(s): {}",
        opts.out.display(),
        plan.mission,
        plan.tasks.len(),
        plan.tasks
            .iter()
            .map(|task| task.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(json!({ "plan": plan, "out": opts.out, "message": message }))
}

/// `susi harness run` options (the CLI's flags, defaults resolved).
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub plan: PathBuf,
    /// The susi target repository (`.` is the working directory).
    pub repo: PathBuf,
    /// Supervisor resume state (`supervisor-state.json` is per mission).
    pub state: PathBuf,
    pub gateway: String,
    pub model: String,
    /// Per-turn deadline, seconds.
    pub timeout: u64,
    pub parallel: usize,
    pub prefix: String,
    pub workers: String,
    pub budget_usd: String,
    pub catalogue: PathBuf,
    pub daily_usd: String,
    pub ledger: PathBuf,
    pub controls: PathBuf,
    pub rsi_log: PathBuf,
    pub attempts: usize,
    pub adapter_state: PathBuf,
    /// The susi binary the adapter drives.
    pub susi: PathBuf,
}

/// The state file a run resumes from: `supervisor-state.json` (the default)
/// is per mission, so two missions in one directory never share progress.
pub fn run_state_path(opts: &RunOptions, mission: &str, cwd: &Path) -> PathBuf {
    if opts.state == Path::new("supervisor-state.json") {
        cwd.join(format!("supervisor-{mission}.json"))
    } else {
        opts.state.clone()
    }
}

/// What a finished `susi harness run` reports.
pub struct RunOutcome {
    /// The supervisor's report, or the error that stopped the run.
    pub report: EaiResult<SupervisorReport>,
    /// The one-line budget report, read after the run.
    pub budget_line: String,
}

/// `susi harness run`: run a plan under the budget gate (prepare in
/// parallel, land in order, persist after each task). Top-up alerts and
/// fleet notes go to `notes` before the first task.
pub fn run(opts: &RunOptions, cwd: &Path, notes: Notes<'_>) -> RunOutcome {
    let (before, spent_before) = gather_budgets(&opts.ledger);
    for line in fleet::alerts(&before) {
        notes(line);
    }
    let report = run_mission(opts, cwd, &before, notes);
    let (after, spent_after) = gather_budgets(&opts.ledger);
    let spend = total_spent(&spent_after)
        .checked_sub(total_spent(&spent_before))
        .unwrap_or(MicroUsd::ZERO);
    RunOutcome {
        report,
        budget_line: after.report_line(spend, Registry::builtin()),
    }
}

/// Why a finished mission counts as failed (`None` when every task landed).
pub fn mission_failure(report: &SupervisorReport) -> Option<String> {
    (!report.failed.is_empty()).then(|| {
        format!(
            "mission {} failed {} task(s)",
            report.mission,
            report.failed.len()
        )
    })
}

fn run_mission(
    opts: &RunOptions,
    cwd: &Path,
    budgets: &Budgets,
    notes: Notes<'_>,
) -> EaiResult<SupervisorReport> {
    let plan = Plan::load(&opts.plan).with_context(|| format!("load {}", opts.plan.display()))?;
    let state_path = run_state_path(opts, &plan.mission, cwd);
    let repo = if opts.repo == Path::new(".") {
        cwd.to_path_buf()
    } else {
        opts.repo.clone()
    };
    let facts = RepoFacts::load(&repo).context("read the workspace's packages")?;
    validate_plan(&plan, &facts).context("validate the plan")?;
    let catalogue = PriceCatalogue::load(&opts.catalogue).context("load the price catalogue")?;
    let Some(budget) = MicroUsd::parse_usd(&opts.budget_usd).map(Budget::new) else {
        return bail(format!(
            "--budget-usd {} is not a dollar amount",
            opts.budget_usd
        ));
    };
    let Some(daily_cap) = MicroUsd::parse_usd(&opts.daily_usd) else {
        return bail(format!(
            "--daily-usd {} is not a dollar amount",
            opts.daily_usd
        ));
    };
    let spent_today = spent_today(&opts.ledger, now_unix_ms(), notes);
    if spent_today.micros() >= daily_cap.micros() {
        return bail(format!(
            "--daily-usd {daily_cap} already spent today ({spent_today}): refusing to start another run"
        ));
    }
    // Every named worker stays on the ladder. The task's own model is moved to
    // the front by the adapter, so a task assigned a non-default worker still
    // has a stronger model to escalate to when it does not pass.
    let evidence = live_evidence(&opts.ledger, cwd, notes);
    let fleet = resolve_fleet(
        &opts.workers,
        || Ok(catalogue.clone()),
        budgets,
        &evidence,
        Some(&opts.gateway),
        notes,
    )?;
    let orchestrator = fleet::resolve_model("auto", &fleet).unwrap_or_else(|_| "none".to_string());
    // A task starts on a light worker: `--model auto` gives the fleet's first,
    // and a named model is used as named. A failure escalates to the fixers.
    let model = if opts.model.trim() == "auto" {
        match fleet.task_workers().first() {
            Some(model) => model.clone(),
            None => return bail("the fleet has no model to start tasks on"),
        }
    } else {
        opts.model.clone()
    };
    notes(format!(
        "fleet: orchestrator {orchestrator}; workers {}; fixers {}",
        fleet.workers.join(", "),
        fleet.fixers.join(", ")
    ));
    if !fleet.standby.is_empty() {
        notes(format!(
            "fleet: in reserve until a probe answers: {}",
            fleet.standby.join(", ")
        ));
    }
    let alternates = mission_alternates(&fleet);
    let repo_for_board = repo.clone();
    let target = SusiTarget::new(repo, opts.susi.clone());
    let native = NativeWorker;
    let executor = SusiExecutor {
        target: &target,
        runner: &native,
        worktree_prefix: opts.prefix.clone(),
        gateway_url: opts.gateway.clone(),
        model,
        alternates,
        catalogue: Some(catalogue),
        budget: Some(budget),
        ledger_path: opts.ledger.clone(),
        mission: plan.mission.clone(),
        policy: Policy {
            max_attempts: opts.attempts.max(1),
            ..Policy::default()
        },
        timeout: Duration::from_secs(opts.timeout),
        state_dir: opts.adapter_state.clone(),
        lifecycle_path: Some(opts.rsi_log.clone()),
        guard: std::sync::OnceLock::new(),
        exhausted: AtomicBool::new(false),
    };
    let agent = susi_core::native_board::resolve_agent(None, Some(&repo_for_board))
        .map_err(EaiError::governance)?;
    let host = susi_core::native_board::resolve_host(None);
    susi_core::native_board::collaborate_and_post(
        Some(&repo_for_board),
        &susi_core::native_board::BoardDraft {
            kind: "claim".to_string(),
            agent: agent.clone(),
            host: host.clone(),
            title: susi_core::native_board::sanitize_title(&format!(
                "harness mission {}",
                plan.mission
            )),
            body: susi_core::native_board::sanitize_body(&format!(
                "repo: {}\ntasks: {}",
                repo_for_board.display(),
                plan.tasks.len()
            )),
            to: None,
            re: None,
            task: Some(plan.mission.clone()),
        },
        true,
    )
    .map_err(EaiError::governance)?;
    let supervisor = Supervisor::new(&state_path, &executor)
        .with_parallelism(opts.parallel)
        .with_controls(&opts.controls);
    let report = supervisor.run(&plan).context("run the mission")?;
    let all_ok = report.failed.is_empty();
    susi_core::native_board::collaborate_and_post(
        Some(&repo_for_board),
        &susi_core::native_board::BoardDraft {
            kind: if all_ok { "landed" } else { "blocked" }.to_string(),
            agent,
            host,
            title: susi_core::native_board::sanitize_title(&format!(
                "harness mission {} {}",
                plan.mission,
                if all_ok { "landed" } else { "blocked" }
            )),
            body: susi_core::native_board::sanitize_body(&format!(
                "failed: {}",
                report.failed.len()
            )),
            to: None,
            re: None,
            task: Some(plan.mission.clone()),
        },
        true,
    )
    .map_err(EaiError::governance)?;
    Ok(report)
}

/// The models a mission's tasks may use: the fleet's ladder, then the models
/// held in reserve for their refusal ([`Fleet::standby`]). A task's order
/// leaves a refused model out until a probe answers; the reserve is what
/// lets the before-each-task probe ([`fleet::readmit`]) reach a model that
/// was refused when the run started, so one fixed or topped up mid-mission
/// comes back without a restart.
pub fn mission_alternates(fleet: &Fleet) -> Vec<String> {
    let mut alternates = fleet.ladder.clone();
    for model in &fleet.standby {
        if !alternates.contains(model) {
            alternates.push(model.clone());
        }
    }
    alternates
}

/// Sum the ledger's priced spend for the UTC day containing `now_unix_ms`.
///
/// The rows are read one line at a time, as bytes
/// ([`crate::ledger::read_rows_lenient`]). Reading the file as one
/// string turned a row torn inside a multi-byte character into "nothing spent
/// today" for every later run, and dropped a real charge glued onto an
/// ASCII tear, so `--daily-usd` let runs start past the cap.
pub fn spent_today(ledger: &Path, now_unix_ms: u64, notes: Notes<'_>) -> MicroUsd {
    let (rows, skipped) = crate::ledger::read_rows_lenient::<Value>(ledger);
    if skipped > 0 {
        notes(format!(
            "⚠ ledger {}: {skipped} line(s) could not be read and were skipped",
            ledger.display()
        ));
    }
    let day = now_unix_ms / 86_400_000;
    let total = rows
        .iter()
        .filter(|entry| {
            entry
                .get("started_unix_ms")
                .and_then(Value::as_u64)
                .is_some_and(|started| started / 86_400_000 == day)
        })
        .filter_map(|entry| entry.get("cost").and_then(Value::as_u64))
        .fold(0u64, u64::saturating_add);
    MicroUsd::from_micros(total)
}

/// Wall clock in milliseconds since the Unix epoch.
pub fn now_unix_ms() -> u64 {
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

/// The fleet for `--workers` ([`fleet::resolve_fleet_through`]), with each
/// model left off named in `notes`. A command that drives `gateway` asks it
/// about a listed model whose key is not set here ([`fleet::gateway_answers`]):
/// a gateway on an explicit upstream, or with its own key, routes it.
#[allow(clippy::too_many_arguments)] // the fleet inputs plus the note sink
pub fn resolve_fleet(
    spec: &str,
    catalogue: impl FnOnce() -> EaiResult<PriceCatalogue>,
    budgets: &Budgets,
    evidence: &fleet::Evidence,
    gateway: Option<&str>,
    notes: Notes<'_>,
) -> EaiResult<Fleet> {
    let ask = gateway.map(|url| move |model: &str| fleet::gateway_answers(url, model));
    let check = ask.as_ref().map(|ask| ask as fleet::GatewayCheck<'_>);
    let fleet = fleet::resolve_fleet_through(spec, catalogue, budgets, evidence, check)
        .map_err(|error| EaiError::internal(format!("--workers {spec}: {error}")))?;
    for note in &fleet.left_off {
        notes(format!("⚠ {note}"));
    }
    Ok(fleet)
}

/// What the real calls say about the models, probing the ones whose last call
/// failed long enough ago ([`fleet::evidence`]); each probe goes to `notes`.
pub fn live_evidence(ledger: &Path, cwd: &Path, notes: Notes<'_>) -> fleet::Evidence {
    let catalogue = default_catalogue(cwd).ok();
    let evidence = fleet::evidence(ledger, catalogue.as_ref(), true);
    for line in fleet::probe_report(&evidence) {
        notes(line);
    }
    evidence
}

/// The price catalogue at the workspace's default location, or the one built
/// into the binary when the workspace has none.
pub fn default_catalogue(cwd: &Path) -> EaiResult<PriceCatalogue> {
    PriceCatalogue::load_or_builtin(&cwd.join("deploy/harness/prices.json"))
}

/// Mandate 73 container capability contracts discovered on this host.
pub fn recursive_contracts() -> EaiResult<Value> {
    Ok(serde_json::to_value(
        crate::supervisor::discover_verified_container_contracts(),
    )?)
}

/// Execute a Mandate 73 [`crate::recursive::RecursiveTaskNode`] tree through the
/// supervisor's live verified container contracts and isolated worker sandboxes.
pub fn run_recursive(
    run: &crate::supervisor::RecursiveTreeRun<'_>,
) -> EaiResult<crate::recursive::ExecutionProvenanceReceipt> {
    crate::supervisor::execute_recursive_tree(run)
        .map_err(|error| EaiError::internal(error.to_string()))
}

/// After a live publication the publishing clone witnesses the storage remote
/// (`crate::storage_guard`): lost history becomes a note, never a failure.
fn witness_storage(repo: &std::path::Path, remote: &str, dry_run: bool, notes: Notes<'_>) {
    if !dry_run {
        if let Some(note) = crate::storage_guard::witness(repo, remote) {
            notes(note);
        }
    }
}

/// Automatically discover and integrate eligible pending development branches
/// into unqualified `<remote>/main` (`susi-harness integrate` / `daemon.harness`
/// `"integrate_pending"`).
pub fn integrate_pending(
    config: &crate::git_governor::AutoMergeConfig,
    notes: Notes<'_>,
) -> EaiResult<Value> {
    let receipt = crate::git_governor::sweep_pending_branches(config)?;
    notes(format!(
        "auto-merge ({}): merged={}, already_merged={}, protected={}, retired={}, deferred_or_refused={}",
        if receipt.dry_run { "dry-run" } else { "live" },
        receipt.merged_count,
        receipt.already_merged_count,
        receipt.protected_stream_count,
        receipt.retired_count,
        receipt.deferred_or_refused_count,
    ));
    witness_storage(&config.repo, &config.remote, receipt.dry_run, notes);
    Ok(serde_json::to_value(receipt)?)
}

/// Reconcile and fast-forward a single revision onto unqualified `<remote>/main`
/// (`susi-harness submit-main` / `daemon.harness` `"submit_main"`).
pub fn submit_main(
    config: &crate::git_governor::SubmitMainConfig,
    notes: Notes<'_>,
) -> EaiResult<Value> {
    let receipt = crate::git_governor::submit_main(config)?;
    notes(format!(
        "submit-main ({}): {} -> {}",
        receipt.mode, receipt.source_oid, receipt.final_main_oid
    ));
    witness_storage(&config.repo, &config.remote, receipt.dry_run, notes);
    Ok(serde_json::to_value(receipt)?)
}

/// Audit `release/*` candidates and optionally promote the latest verified
/// distillation candidate (`susi-harness release-promote` / `daemon.harness`
/// `"promote_release"`).
pub fn promote_release(
    config: &crate::git_governor::ReleasePromotionConfig,
    notes: Notes<'_>,
) -> EaiResult<Value> {
    let receipt = crate::git_governor::promote_release_candidate(config)?;
    notes(format!(
        "release-promote ({}): latest_verified={:?}, promoted={:?}",
        if receipt.dry_run { "dry-run" } else { "live" },
        receipt.latest_verified_branch,
        receipt.promoted_branch_ref,
    ));
    witness_storage(&config.repo, &config.remote, receipt.dry_run, notes);
    Ok(serde_json::to_value(receipt)?)
}

/// Preserve and retire non-`main` remote `refs/heads/*` branch clutter on `<remote>`
/// after confirming immutable `refs/tags/susi-distill/legacy-*` preservation
/// (`susi-harness retire-branches`).
pub fn retire_branches(
    config: &crate::git_governor::RetireBranchesConfig,
    notes: Notes<'_>,
) -> EaiResult<Value> {
    let receipt = crate::git_governor::retire_remote_clutter(config)?;
    notes(format!(
        "retire-branches ({}): retired={}, skipped={}, refused={}, remaining_remote_heads={}",
        if receipt.dry_run { "dry-run" } else { "live" },
        receipt.retired_count,
        receipt.skipped_count,
        receipt.refused_count,
        receipt.remaining_remote_head_count,
    ));
    Ok(serde_json::to_value(receipt)?)
}

#[cfg(test)]
#[path = "tests/operator.rs"]
mod tests;
