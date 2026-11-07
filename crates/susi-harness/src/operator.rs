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

impl PlanQueueOptions {
    /// Verify structural invariants of [`PlanQueueOptions`].
    ///
    /// # Errors
    ///
    /// Returns an explanation string when paths or strings are blank, `limit == 0`,
    /// or `gateway` is present without an `http://` or `https://` scheme.
    pub fn verify_plan_options_invariants(&self) -> Result<(), String> {
        if self.repo.as_os_str().is_empty() {
            return Err("plan options repo path must not be empty".to_string());
        }
        if self.out.as_os_str().is_empty() {
            return Err("plan options out path must not be empty".to_string());
        }
        if self.mission.trim().is_empty() {
            return Err("plan options mission must not be empty".to_string());
        }
        if self.limit == 0 {
            return Err("plan options limit must be greater than zero".to_string());
        }
        if self.workers.trim().is_empty() {
            return Err("plan options workers must not be empty".to_string());
        }
        if let Some(ref roadmap) = self.roadmap {
            if roadmap.trim().is_empty() {
                return Err("plan options roadmap must not be blank when present".to_string());
            }
        }
        if let Some(ref gw) = self.gateway {
            let trimmed = gw.trim();
            if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                return Err(format!(
                    "plan options gateway '{gw}' must start with http:// or https://"
                ));
            }
        }
        Ok(())
    }
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

impl AuthorOptions {
    /// Verify structural invariants of [`AuthorOptions`].
    ///
    /// # Errors
    ///
    /// Returns an explanation string when `gateway` lacks an HTTP/HTTPS scheme,
    /// `model`, `vector`, or `workers` is blank, `roadmap` or `out` is empty,
    /// or `max_tasks == 0`.
    pub fn verify_author_options_invariants(&self) -> Result<(), String> {
        let gw = self.gateway.trim();
        if !gw.starts_with("http://") && !gw.starts_with("https://") {
            return Err(format!(
                "author options gateway '{}' must start with http:// or https://",
                self.gateway
            ));
        }
        if self.model.trim().is_empty() {
            return Err("author options model must not be empty".to_string());
        }
        if self.roadmap.as_os_str().is_empty() {
            return Err("author options roadmap path must not be empty".to_string());
        }
        if self.vector.trim().is_empty() {
            return Err("author options vector must not be empty".to_string());
        }
        if self.out.as_os_str().is_empty() {
            return Err("author options out path must not be empty".to_string());
        }
        if self.max_tasks == 0 {
            return Err("author options max_tasks must be greater than zero".to_string());
        }
        if self.workers.trim().is_empty() {
            return Err("author options workers must not be empty".to_string());
        }
        Ok(())
    }
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

impl RunOptions {
    /// Verify structural and budget invariants of [`RunOptions`].
    ///
    /// # Errors
    ///
    /// Returns an explanation string when any required path or string is empty,
    /// `gateway` lacks an HTTP/HTTPS scheme, `timeout`, `parallel`, or `attempts`
    /// is zero, or `budget_usd` / `daily_usd` is not a positive USD amount.
    pub fn verify_run_options_invariants(&self) -> Result<(), String> {
        for (name, path) in [
            ("plan", &self.plan),
            ("repo", &self.repo),
            ("state", &self.state),
            ("catalogue", &self.catalogue),
            ("ledger", &self.ledger),
            ("controls", &self.controls),
            ("rsi_log", &self.rsi_log),
            ("adapter_state", &self.adapter_state),
            ("susi", &self.susi),
        ] {
            if path.as_os_str().is_empty() {
                return Err(format!("run options '{name}' path must not be empty"));
            }
        }
        let gw = self.gateway.trim();
        if !gw.starts_with("http://") && !gw.starts_with("https://") {
            return Err(format!(
                "run options gateway '{}' must start with http:// or https://",
                self.gateway
            ));
        }
        if self.model.trim().is_empty()
            || self.prefix.trim().is_empty()
            || self.workers.trim().is_empty()
        {
            return Err("run options model, prefix, and workers must not be empty".to_string());
        }
        if self.timeout == 0 || self.parallel == 0 || self.attempts == 0 {
            return Err("run options timeout, parallel, and attempts must be > 0".to_string());
        }
        if !MicroUsd::parse_usd(&self.budget_usd).is_some_and(|b| !b.is_zero()) {
            return Err(format!(
                "run options budget_usd '{}' must be a positive USD amount",
                self.budget_usd
            ));
        }
        if !MicroUsd::parse_usd(&self.daily_usd).is_some_and(|d| !d.is_zero()) {
            return Err(format!(
                "run options daily_usd '{}' must be a positive USD amount",
                self.daily_usd
            ));
        }
        Ok(())
    }
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
    let supervisor = Supervisor::new(&state_path, &executor)
        .with_parallelism(opts.parallel)
        .with_controls(&opts.controls);
    supervisor.run(&plan).context("run the mission")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_json_lists_every_model_once_per_provider() {
        let doc = registry_json();
        let providers = doc["providers"].as_array().unwrap();
        let flat = doc["models"].as_array().unwrap();
        let nested: usize = providers
            .iter()
            .map(|p| p["models"].as_array().unwrap().len())
            .sum();
        assert_eq!(nested, flat.len());
        assert!(doc["aliases"].is_object() || doc["aliases"].is_array());
    }

    #[test]
    fn verify_failures_names_only_failed_providers() {
        let report = json!({"ok": false, "providers": [
            {"provider": "a", "ok": true},
            {"provider": "b", "ok": false},
        ]});
        assert_eq!(verify_failures(&report), vec!["b".to_string()]);
        assert!(verify_failures(&json!({"ok": true, "providers": []})).is_empty());
    }

    // `budget` and `run` read provider balances from their endpoints
    // (`fleet::gather_budgets`), so unit tests stay on the pure helpers.
    #[test]
    fn spent_today_sums_only_today_and_reports_torn_lines() {
        let dir = std::env::temp_dir().join(format!("susi-harness-op-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("ledger.jsonl");
        let now = 86_400_000 * 20_000 + 5;
        std::fs::write(
            &ledger,
            format!(
                "{}\n{}\nnot json\n",
                json!({"started_unix_ms": now, "cost": 7}),
                json!({"started_unix_ms": 5, "cost": 100}),
            ),
        )
        .unwrap();
        let mut seen = Vec::new();
        let spent = spent_today(&ledger, now, &mut |n: String| seen.push(n));
        assert_eq!(spent.micros(), 7);
        assert_eq!(seen.len(), 1, "the torn line is reported: {seen:?}");
        assert_eq!(resolve(&dir, Path::new("ledger.jsonl")), ledger);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_state_is_per_mission_by_default() {
        let opts = RunOptions {
            plan: "plan.json".into(),
            repo: ".".into(),
            state: "supervisor-state.json".into(),
            gateway: "http://127.0.0.1:1".into(),
            model: "auto".into(),
            timeout: 1,
            parallel: 1,
            prefix: "h".into(),
            workers: "auto".into(),
            budget_usd: "1".into(),
            catalogue: "c.json".into(),
            daily_usd: "1".into(),
            ledger: "l.jsonl".into(),
            controls: "c.jsonl".into(),
            rsi_log: "r.jsonl".into(),
            attempts: 1,
            adapter_state: "s".into(),
            susi: "susi".into(),
        };
        assert_eq!(
            run_state_path(&opts, "m1", Path::new("/w")),
            Path::new("/w").join("supervisor-m1.json")
        );
        let mut named = opts.clone();
        named.state = "mine.json".into();
        assert_eq!(
            run_state_path(&named, "m1", Path::new("/w")),
            Path::new("mine.json")
        );
    }

    #[test]
    fn mission_failure_and_mission_alternates_and_tail_behave_deterministically() {
        let mut report = SupervisorReport {
            mission: "m-op".into(),
            done: vec!["t1".into()],
            failed: Vec::new(),
            skipped: Vec::new(),
            unstarted: Vec::new(),
            budget_exhausted: false,
            paused: false,
            aborted: false,
        };
        assert_eq!(mission_failure(&report), None);
        report.failed.push(("t2".into(), "reason".into()));
        assert_eq!(
            mission_failure(&report).as_deref(),
            Some("mission m-op failed 1 task(s)")
        );

        let fleet = Fleet {
            orchestrator: Some("gemini-pro".into()),
            workers: vec!["deepseek-flash".into()],
            fixers: vec!["claude-sonnet".into()],
            ladder: vec!["deepseek-flash".into(), "claude-sonnet".into()],
            standby: vec!["claude-sonnet".into(), "gpt-sol".into()],
            left_off: Vec::new(),
        };
        assert_eq!(
            mission_alternates(&fleet),
            vec![
                "deepseek-flash".to_string(),
                "claude-sonnet".to_string(),
                "gpt-sol".to_string()
            ]
        );
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("abcdef", 3), "def");
        assert!(now_unix_ms() > 0);
    }

    #[test]
    fn operator_plan_author_and_run_options_verify_invariants_reject_corrupt_state() {
        let valid_plan = PlanQueueOptions {
            repo: ".".into(),
            out: "plan.json".into(),
            mission: "m-1".into(),
            limit: 4,
            workers: "auto".into(),
            roadmap: Some("VC-203-001".into()),
            gateway: Some("http://127.0.0.1:8080".into()),
        };
        assert!(valid_plan.verify_plan_options_invariants().is_ok());

        let mut bad_plan = valid_plan.clone();
        bad_plan.limit = 0;
        assert!(bad_plan.verify_plan_options_invariants().is_err());

        let mut bad_plan_gw = valid_plan;
        bad_plan_gw.gateway = Some("ftp://127.0.0.1".into());
        assert!(bad_plan_gw.verify_plan_options_invariants().is_err());

        let valid_author = AuthorOptions {
            gateway: "http://127.0.0.1:8080".into(),
            model: "auto".into(),
            roadmap: "roadmap.json".into(),
            vector: "VC-203-001".into(),
            out: "plan.json".into(),
            max_tasks: 5,
            workers: "auto".into(),
        };
        assert!(valid_author.verify_author_options_invariants().is_ok());

        let mut bad_author = valid_author;
        bad_author.max_tasks = 0;
        assert!(bad_author.verify_author_options_invariants().is_err());

        let valid_run = RunOptions {
            plan: "plan.json".into(),
            repo: ".".into(),
            state: "supervisor-state.json".into(),
            gateway: "http://127.0.0.1:8080".into(),
            model: "auto".into(),
            timeout: 120,
            parallel: 2,
            prefix: "h".into(),
            workers: "auto".into(),
            budget_usd: "5.00".into(),
            catalogue: "prices.json".into(),
            daily_usd: "25.00".into(),
            ledger: "ledger.jsonl".into(),
            controls: "controls.jsonl".into(),
            rsi_log: "rsi.jsonl".into(),
            attempts: 2,
            adapter_state: "state".into(),
            susi: "susi".into(),
        };
        assert!(valid_run.verify_run_options_invariants().is_ok());

        let mut zero_budget = valid_run.clone();
        zero_budget.budget_usd = "0.00".into();
        assert!(zero_budget.verify_run_options_invariants().is_err());

        let mut zero_parallel = valid_run;
        zero_parallel.parallel = 0;
        assert!(zero_parallel.verify_run_options_invariants().is_err());
    }
}
