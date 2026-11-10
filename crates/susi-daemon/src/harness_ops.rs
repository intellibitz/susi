//! The self-build harness over the socket: the `daemon.harness` plane topic
//! behind the MCP `harness_status`, `harness_budget`, `harness_models`,
//! `harness_plan`, `harness_author`, `harness_run` and `harness_runs` tools
//! (zero-UI plan B4 group 1), unified with Mandate 73 recursive delegation
//! (`susi_core::delegation`) and container execution (`susi_harness::recursive`).
//! Every action calls [`susi_harness::operator`], the library `susi harness` calls,
//! inside an attenuated [`DelegationScope`].
//!
//! The socket has no caller working directory, so every path is absolute
//! and `repo` names the target repository (the CLI's working directory).
//! A run is long: `run` starts it on a daemon thread and returns at once;
//! `runs` and `status` (with the run's `state`) report its progress.

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use susi_core::delegation::{
    DelegationAuthority, DelegationEnvelope, DelegationHop, DelegationLimits, DelegationRequest,
    DelegationScope,
};
use susi_harness::fleet;
use susi_harness::money::MicroUsd;
use susi_harness::operator::{self, ModelsMode};
use susi_harness::recursive::{
    AuthorityGrant, CancellationToken, ExecutionProvenanceReceipt, RecursiveTaskNode,
};
use susi_harness::supervisor::{Plan, RecursiveTreeRun};

type R<T> = Result<T, String>;

const PERM_HARNESS_READ: &str = "harness:read";
const PERM_HARNESS_PLAN: &str = "harness:plan";
const PERM_HARNESS_RUN: &str = "harness:run";

fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn number(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

/// An absolute path argument (the socket has no caller working directory).
fn absolute(payload: &Value, key: &str) -> R<Option<PathBuf>> {
    let Some(text) = field(payload, key) else {
        return Ok(None);
    };
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        ));
    }
    Ok(Some(path))
}

fn required_path(payload: &Value, key: &str) -> R<PathBuf> {
    absolute(payload, key)?.ok_or_else(|| format!("{key} is required"))
}

/// The repository the action reads (the CLI's working directory): `repo`
/// when given, else the substrate home.
fn repo_or_home(payload: &Value) -> R<PathBuf> {
    Ok(absolute(payload, "repo")?.unwrap_or_else(susi_paths::SusiDirs::substrate_home))
}

fn scope_path_strings(scope: &DelegationScope) -> Vec<String> {
    scope.path().iter().map(ToString::to_string).collect()
}

fn with_notes_and_path(
    doc: R<Value>,
    notes: Vec<String>,
    delegation_path: Vec<String>,
) -> R<Value> {
    let mut doc = doc?;
    if let Some(map) = doc.as_object_mut() {
        map.insert("notes".into(), json!(notes));
        map.insert("delegation_path".into(), json!(delegation_path));
        return Ok(doc);
    }
    Ok(json!({
        "result": doc,
        "notes": notes,
        "delegation_path": delegation_path,
    }))
}

fn required_action_permission(action: &str) -> Option<&'static str> {
    match action {
        "status" | "budget" | "models" | "runs" | "recursive_contracts" => Some(PERM_HARNESS_READ),
        "plan" | "author" => Some(PERM_HARNESS_PLAN),
        "run" | "run_recursive" | "cancel_run" | "revoke_delegation" | "integrate_pending"
        | "submit_main" | "promote_release" => Some(PERM_HARNESS_RUN),
        _ => None,
    }
}

fn default_root_permissions(extra: &BTreeSet<String>) -> BTreeSet<String> {
    let mut perms: BTreeSet<String> = [
        PERM_HARNESS_READ,
        PERM_HARNESS_PLAN,
        PERM_HARNESS_RUN,
        "net:outbound",
        "git:publish",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    perms.extend(extra.iter().cloned());
    perms
}

fn grant_permissions(grant: &AuthorityGrant) -> R<BTreeSet<String>> {
    let mut perms = BTreeSet::new();
    perms.insert(PERM_HARNESS_RUN.to_string());
    if grant.allow_network {
        perms.insert("net:outbound".to_string());
    }
    if grant.allow_git_publish {
        perms.insert("git:publish".to_string());
    }
    for cap in &grant.allowed_capabilities {
        let trimmed = cap.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
            return Err(format!("invalid recursive capability {cap:?}"));
        }
        perms.insert(format!("cap:{trimmed}"));
    }
    Ok(perms)
}

fn collect_tree_permissions(node: &RecursiveTaskNode, out: &mut BTreeSet<String>) -> R<()> {
    out.extend(grant_permissions(&node.grant)?);
    for wave in &node.child_waves {
        for child in wave {
            collect_tree_permissions(child, out)?;
        }
    }
    Ok(())
}

/// Open a [`DelegationScope`] for one `daemon.harness` action, nested under the
/// caller's signed `susi-delegation/v1` envelope when provided or under a root
/// operator scope when omitted.
fn open_action_scope(
    payload: &Value,
    action: &str,
    required_perm: &str,
    extra_perms: &BTreeSet<String>,
    authority_override: Option<&DelegationAuthority>,
) -> R<DelegationScope> {
    let subject = field(payload, "subject").unwrap_or("daemon-operator");
    let timeout_ms = number(payload, "timeout_ms").unwrap_or(900_000);
    let requested_budget = match action {
        "run" => payload
            .get("budget_usd")
            .and_then(Value::as_str)
            .and_then(MicroUsd::parse_usd)
            .map_or(0, MicroUsd::micros),
        _ => number(payload, "budget_micros").unwrap_or(0),
    };

    let parent = match payload.get("delegation").filter(|v| !v.is_null()) {
        Some(raw_env) => {
            let envelope = DelegationEnvelope::deserialize(raw_env)
                .map_err(|e| format!("invalid delegation envelope: {e}"))?;
            match authority_override {
                Some(auth) => auth.admit(&envelope).map_err(msg)?,
                None => DelegationAuthority::host()
                    .and_then(|auth| auth.admit(&envelope))
                    .map_err(msg)?,
            }
        }
        None => {
            let root_hop = DelegationHop::new(subject, "root").map_err(msg)?;
            let perms = default_root_permissions(extra_perms);
            DelegationScope::root(root_hop, perms, u64::MAX, None, DelegationLimits::default())
                .map_err(msg)?
        }
    };

    let mut child_perms = extra_perms.clone();
    child_perms.insert(required_perm.to_string());
    let hop = DelegationHop::new(subject, format!("daemon.harness.{action}")).map_err(msg)?;
    parent
        .delegate(DelegationRequest {
            hop,
            permissions: child_perms,
            budget_micros: requested_budget,
            timeout_ms,
        })
        .map_err(msg)
}

/// Validate and reserve [`DelegationScope`]s across a [`RecursiveTaskNode`] wave
/// hierarchy so `susi_core::delegation` is the single authority for permissions,
/// micro-USD sibling budget reservation, deadlines, depth, fan-out, cycles, and
/// ancestor cancellation.
pub fn preflight_recursive_delegation(
    parent: &DelegationScope,
    node: &RecursiveTaskNode,
    cancel: &CancellationToken,
) -> R<DelegationScope> {
    parent.check().map_err(|e| {
        cancel.cancel();
        e.to_string()
    })?;
    let hop = DelegationHop::new(
        node.component_id.clone(),
        format!("harness.task.{}", node.task_key),
    )
    .map_err(msg)?;
    let frame_scope = parent
        .delegate(DelegationRequest {
            hop,
            permissions: grant_permissions(&node.grant)?,
            budget_micros: node.grant.budget_cap.micros(),
            timeout_ms: node.grant.timeout_ms,
        })
        .map_err(msg)?;

    for wave in &node.child_waves {
        // Keep all concurrent siblings in this wave open together so sibling
        // reservations cannot double-spend the parent's `remaining_micros`.
        let mut live_wave_scopes = Vec::with_capacity(wave.len());
        for child in wave {
            let child_scope = preflight_recursive_delegation(&frame_scope, child, cancel)?;
            live_wave_scopes.push(child_scope);
        }
        drop(live_wave_scopes);
    }
    Ok(frame_scope)
}

/// Execute a [`RecursiveTaskNode`] tree layered over `parent` [`DelegationScope`].
pub fn run_recursive_in_scope(
    parent: &DelegationScope,
    payload: &Value,
    node: &RecursiveTaskNode,
) -> R<(ExecutionProvenanceReceipt, Vec<String>)> {
    let cancel = CancellationToken::new();
    let tree_scope = preflight_recursive_delegation(parent, node, &cancel)?;
    let sandbox_root = match absolute(payload, "sandbox_root")? {
        Some(dir) => dir,
        None => repo_or_home(payload)?.join("harness-recursive-sandboxes"),
    };
    let gateway_url = field(payload, "gateway").unwrap_or("http://127.0.0.1:9185");
    let default_model = field(payload, "model").unwrap_or("auto");
    let root_run_id = field(payload, "run_id").unwrap_or(&node.task_key);
    let port_offset = number(payload, "port_offset")
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(100);

    let run = RecursiveTreeRun {
        sandbox_root: &sandbox_root,
        gateway_url,
        default_model,
        root_run_id,
        node,
        base_port_offset: port_offset,
        cancel: &cancel,
    };
    let receipt = operator::run_recursive(&run).map_err(msg)?;
    tree_scope.check().map_err(|e| {
        cancel.cancel();
        e.to_string()
    })?;
    tree_scope
        .charge(receipt.total_spent.micros())
        .map_err(msg)?;
    let path = scope_path_strings(&tree_scope);
    Ok((receipt, path))
}

/// One mission the daemon is running or ran.
#[derive(Debug, Clone)]
struct RunRecord {
    state: PathBuf,
    started_unix_ms: u64,
    notes: Vec<String>,
    delegation_path: Vec<String>,
    scope: DelegationScope,
    cancel: CancellationToken,
    /// `None` while running; the report (or error) and budget line once done.
    outcome: Option<Value>,
}

fn runs() -> &'static Mutex<BTreeMap<String, RunRecord>> {
    static RUNS: OnceLock<Mutex<BTreeMap<String, RunRecord>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn run_view(mission: &str, record: &RunRecord) -> Value {
    json!({
        "mission": mission,
        "state": record.state,
        "started_unix_ms": record.started_unix_ms,
        "running": record.outcome.is_none(),
        "cancelled": record.cancel.is_cancelled() || record.scope.is_cancelled(),
        "delegation_path": record.delegation_path,
        "notes": record.notes,
        "outcome": record.outcome,
    })
}

/// The missions this daemon has started, newest state last.
pub fn list_runs() -> Value {
    let runs = runs().lock().unwrap_or_else(|e| e.into_inner());
    Value::Array(runs.iter().map(|(m, r)| run_view(m, r)).collect())
}

/// Cancel a mission's [`DelegationScope`] and cooperative [`CancellationToken`].
pub fn cancel_mission_run(mission: &str) -> bool {
    let table = runs().lock().unwrap_or_else(|e| e.into_inner());
    let Some(record) = table.get(mission) else {
        return false;
    };
    record.scope.cancel();
    record.cancel.cancel();
    true
}

fn sync_revoked_runs() -> usize {
    let table = runs().lock().unwrap_or_else(|e| e.into_inner());
    let mut propagated = 0usize;
    for record in table.values() {
        if record.scope.is_cancelled() && !record.cancel.is_cancelled() {
            record.cancel.cancel();
            propagated += 1;
        }
    }
    propagated
}

/// The `susi harness run` options for a socket request, with the CLI's
/// defaults resolved against `repo` (the CLI resolves them against its
/// working directory, which is the repository).
pub fn run_options(payload: &Value) -> R<operator::RunOptions> {
    let repo = required_path(payload, "repo")?;
    let text = |key: &str, default: &str| field(payload, key).unwrap_or(default).to_string();
    let path = |key: &str, default: &str| -> R<PathBuf> {
        Ok(absolute(payload, key)?.unwrap_or_else(|| repo.join(default)))
    };
    // A socket run always drives this daemon's own binary: the CLI's
    // `--susi` is a local operator choice, and carrying it over the socket
    // would let any bearer holder launch an arbitrary program per task.
    if payload.get("susi").is_some_and(|v| !v.is_null()) {
        return Err(
            "susi cannot be chosen over the socket; a socket run drives the daemon's own binary"
                .to_string(),
        );
    }
    let susi = crate::supervisor::reexec_path()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)
        .map_err(|e| format!("locate the susi binary the adapter drives: {e}"))?;
    Ok(operator::RunOptions {
        plan: required_path(payload, "plan")?,
        state: path("state", "supervisor-state.json")?,
        gateway: field(payload, "gateway")
            .ok_or("gateway is required")?
            .to_string(),
        model: text("model", "auto"),
        timeout: number(payload, "timeout").unwrap_or(900),
        parallel: number(payload, "parallel").map_or(1, |n| n.max(1) as usize),
        prefix: text("prefix", "harness"),
        workers: text("workers", "auto"),
        budget_usd: text("budget_usd", "5"),
        catalogue: path("catalogue", "deploy/harness/prices.json")?,
        daily_usd: text("daily_usd", "25"),
        ledger: absolute(payload, "ledger")?.unwrap_or_else(|| fleet::default_ledger(&repo)),
        controls: path("controls", "out/control.jsonl")?,
        rsi_log: path("rsi_log", ".agents/rsi-loop.jsonl")?,
        attempts: number(payload, "attempts").map_or(3, |n| n as usize),
        adapter_state: path("adapter_state", "harness-state")?,
        susi,
        repo,
    })
}

/// Finalizes a mission's record when its worker unwinds without having
/// recorded an outcome, so a panicked run never reads as running forever
/// (which would also refuse every later run of that mission).
struct FinalizeOnUnwind<'a> {
    mission: &'a str,
}

impl Drop for FinalizeOnUnwind<'_> {
    fn drop(&mut self) {
        let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = table.get_mut(self.mission)
            && record.outcome.is_none()
        {
            record.outcome = Some(json!({
                "result": { "error": "worker panicked before recording an outcome" },
                "budget_line": Value::Null,
            }));
        }
    }
}

/// Run one mission's work and record its notes and outcome; the record is
/// finalized even when `work` panics.
fn run_worker(mission: &str, work: impl FnOnce() -> (Vec<String>, Value)) {
    let _guard = FinalizeOnUnwind { mission };
    let (notes, outcome) = work();
    let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(record) = table.get_mut(mission) {
        record.notes = notes;
        record.outcome = Some(outcome);
    }
}

fn standalone_run_scope() -> R<DelegationScope> {
    let hop = DelegationHop::new("daemon-operator", "daemon.harness.run").map_err(msg)?;
    DelegationScope::root(
        hop,
        [PERM_HARNESS_RUN.to_string()],
        u64::MAX,
        None,
        DelegationLimits::default(),
    )
    .map_err(msg)
}

/// Start `susi harness run` on a daemon thread; refuses a mission this
/// daemon is still running.
pub fn start_run(opts: operator::RunOptions) -> R<Value> {
    let scope = standalone_run_scope()?;
    start_run_in_scope(opts, scope)
}

/// Start `susi harness run` under `scope` on a daemon thread.
pub fn start_run_in_scope(opts: operator::RunOptions, scope: DelegationScope) -> R<Value> {
    scope.check().map_err(msg)?;
    let plan = Plan::load(&opts.plan).map_err(|e| format!("load {}: {e}", opts.plan.display()))?;
    let mission = plan.mission.clone();
    let mission_hop =
        DelegationHop::new("susi-harness", format!("mission.{mission}")).map_err(msg)?;
    let budget_micros = MicroUsd::parse_usd(&opts.budget_usd).map_or(0, MicroUsd::micros);
    let mission_scope = scope
        .delegate(DelegationRequest {
            hop: mission_hop,
            permissions: [PERM_HARNESS_RUN.to_string()].into_iter().collect(),
            budget_micros,
            timeout_ms: opts.timeout.saturating_mul(1_000).max(1),
        })
        .map_err(msg)?;
    let delegation_path = scope_path_strings(&mission_scope);
    let cancel = CancellationToken::new();

    // The per-mission default state file sits in the repository (the CLI's
    // working directory).
    let state = operator::run_state_path(&opts, &mission, &opts.repo);
    {
        let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
        if table.get(&mission).is_some_and(|r| r.outcome.is_none()) {
            return Err(format!(
                "mission {mission} is already running in this daemon"
            ));
        }
        table.insert(
            mission.clone(),
            RunRecord {
                state: state.clone(),
                started_unix_ms: operator::now_unix_ms(),
                notes: Vec::new(),
                delegation_path: delegation_path.clone(),
                scope: mission_scope.clone(),
                cancel: cancel.clone(),
                outcome: None,
            },
        );
    }
    let thread_mission = mission.clone();
    // A one-shot worker on the supervision registry (VC-202-010): status-
    // visible, and never restarted — a mission resumes from its state file
    // on the operator's next run instead.
    let mut job = Some(move || {
        run_worker(&thread_mission, || {
            if let Err(cancelled) = mission_scope.check() {
                cancel.cancel();
                return (
                    Vec::new(),
                    json!({
                        "result": { "error": cancelled.to_string() },
                        "budget_line": Value::Null,
                    }),
                );
            }
            let repo = opts.repo.clone();
            let mut notes = Vec::new();
            let outcome = operator::run(&opts, &repo, &mut |line: String| notes.push(line));
            let report = match outcome.report {
                Ok(report) => {
                    if let Err(cancelled) = mission_scope.check() {
                        cancel.cancel();
                        json!({ "error": cancelled.to_string() })
                    } else {
                        let failure = operator::mission_failure(&report);
                        json!({ "report": report, "failure": failure })
                    }
                }
                Err(error) => json!({ "error": error.to_string() }),
            };
            let outcome = json!({ "result": report, "budget_line": outcome.budget_line });
            (notes, outcome)
        });
    });
    crate::service_supervision::Supervisor::global().spawn_oneshot(
        &format!("harness-run:{mission}"),
        0,
        move || job.take(),
    );
    Ok(json!({
        "mission": mission,
        "state": state,
        "started": true,
        "delegation_path": delegation_path,
    }))
}

fn models_mode(name: Option<&str>) -> R<ModelsMode> {
    match name.unwrap_or("registry") {
        "registry" => Ok(ModelsMode::Registry),
        "verify" => Ok(ModelsMode::Verify),
        "discover" => Ok(ModelsMode::Discover),
        "available" => Ok(ModelsMode::Available),
        other => Err(format!(
            "unknown models mode `{other}` (expected registry, verify, discover or available)"
        )),
    }
}

/// Dispatch one `daemon.harness` request: `{action, ..}`.
pub fn dispatch(payload: &Value) -> R<Value> {
    dispatch_with_authority(payload, None)
}

/// Dispatch one `daemon.harness` request with an optional explicit
/// [`DelegationAuthority`] (used by isolated tests; production passes `None`
/// for [`DelegationAuthority::host`]).
pub fn dispatch_with_authority(
    payload: &Value,
    authority_override: Option<&DelegationAuthority>,
) -> R<Value> {
    if let Some(error) = susi_harness::providers::Registry::overlay_error() {
        return Err(error.to_string());
    }
    let action = field(payload, "action").ok_or("action is required")?;
    let Some(required_perm) = required_action_permission(action) else {
        return Err(format!("unknown harness action `{action}`"));
    };

    let parsed_tree = if action == "run_recursive" {
        let raw_tree = payload
            .get("tree")
            .ok_or_else(|| "tree is required for run_recursive".to_string())?;
        Some(
            RecursiveTaskNode::deserialize(raw_tree)
                .map_err(|e| format!("invalid recursive task tree: {e}"))?,
        )
    } else {
        None
    };

    let mut extra_perms = BTreeSet::new();
    if let Some(ref tree) = parsed_tree {
        collect_tree_permissions(tree, &mut extra_perms)?;
    }
    let dry_run_flag = payload
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if matches!(
        action,
        "integrate_pending" | "submit_main" | "promote_release"
    ) && !dry_run_flag
    {
        extra_perms.insert("git:publish".to_string());
    }

    let action_scope = open_action_scope(
        payload,
        action,
        required_perm,
        &extra_perms,
        authority_override,
    )?;
    let delegation_path = scope_path_strings(&action_scope);

    let mut notes = Vec::new();
    let mut sink = |line: String| notes.push(line);
    let doc = match action {
        "status" => operator::status(
            required_path(payload, "ledger")?,
            absolute(payload, "events")?,
            absolute(payload, "state")?,
            absolute(payload, "plan")?,
        )
        .map_err(msg),
        "budget" => {
            let repo = repo_or_home(payload)?;
            let ledger =
                absolute(payload, "ledger")?.unwrap_or_else(|| fleet::default_ledger(&repo));
            Ok(operator::budget(&ledger, &repo, &mut sink))
        }
        "models" => {
            let mode = models_mode(field(payload, "mode"))?;
            let provider = field(payload, "provider");
            if provider.is_some() && !matches!(mode, ModelsMode::Verify | ModelsMode::Discover) {
                return Err("provider requires mode verify or discover".to_string());
            }
            let repo = repo_or_home(payload)?;
            operator::models(mode, provider, &repo, &mut sink)
                .map(|doc| {
                    if mode == ModelsMode::Verify {
                        json!({ "report": doc.clone(), "failed": operator::verify_failures(&doc) })
                    } else {
                        doc
                    }
                })
                .map_err(msg)
        }
        "plan" => {
            let repo = required_path(payload, "repo")?;
            let opts = operator::PlanQueueOptions {
                out: required_path(payload, "out")?,
                mission: field(payload, "mission")
                    .unwrap_or("roadmap-batch")
                    .to_string(),
                limit: number(payload, "limit").map_or(1, |n| n as usize),
                workers: field(payload, "workers").unwrap_or("auto").to_string(),
                roadmap: field(payload, "roadmap").map(str::to_string),
                gateway: field(payload, "gateway").map(str::to_string),
                repo: repo.clone(),
            };
            operator::plan_queue(&opts, &repo, &mut sink).map_err(msg)
        }
        "author" => {
            let repo = required_path(payload, "repo")?;
            let opts = operator::AuthorOptions {
                gateway: field(payload, "gateway")
                    .ok_or("gateway is required")?
                    .to_string(),
                model: field(payload, "model").unwrap_or("auto").to_string(),
                roadmap: absolute(payload, "roadmap")?
                    .unwrap_or_else(|| repo.join(".agents/roadmap.json")),
                vector: field(payload, "vector")
                    .ok_or("vector is required")?
                    .to_string(),
                out: required_path(payload, "out")?,
                max_tasks: number(payload, "max_tasks").map_or(3, |n| n as usize),
                workers: field(payload, "workers").unwrap_or("auto").to_string(),
            };
            operator::author(&opts, &repo, &mut sink).map_err(msg)
        }
        "run" => start_run_in_scope(run_options(payload)?, action_scope),
        "runs" => Ok(list_runs()),
        "recursive_contracts" => operator::recursive_contracts()
            .map(|contracts| json!({ "contracts": contracts }))
            .map_err(msg),
        "run_recursive" => {
            let Some(ref tree) = parsed_tree else {
                return Err("tree is required for run_recursive".to_string());
            };
            let (receipt, tree_path) = run_recursive_in_scope(&action_scope, payload, tree)?;
            Ok(json!({
                "receipt": receipt,
                "tree_delegation_path": tree_path,
            }))
        }
        "cancel_run" => {
            let mission = field(payload, "mission").ok_or("mission is required")?;
            let cancelled = cancel_mission_run(mission);
            Ok(json!({ "mission": mission, "cancelled": cancelled }))
        }
        "revoke_delegation" => {
            let id = field(payload, "id").ok_or("id is required")?;
            let revoked = match authority_override {
                Some(auth) => auth.revoke(id),
                None => DelegationAuthority::host()
                    .map(|auth| auth.revoke(id))
                    .map_err(msg)?,
            };
            let cancelled_runs = sync_revoked_runs();
            Ok(json!({
                "id": id,
                "revoked": revoked,
                "cancelled_runs": cancelled_runs,
            }))
        }
        "integrate_pending" => {
            let repo = required_path(payload, "repo")?;
            let branch_filters: Vec<String> = payload["branches"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let raw_adopted: Vec<String> = payload["adopt_host_commits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let adopted_host_commits =
                susi_harness::git_governor::parse_adopted_oids(&raw_adopted).map_err(msg)?;
            let cfg = susi_harness::git_governor::AutoMergeConfig {
                repo,
                remote: field(payload, "remote").unwrap_or("origin").to_string(),
                fetch_remote: payload
                    .get("fetch_remote")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                include_local: payload
                    .get("include_local")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                branch_filters,
                adopted_host_commits,
                require_task_trailer: payload
                    .get("require_task_trailer")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                dry_run: dry_run_flag,
                max_attempts: number(payload, "max_attempts").map_or(4, |n| n as usize),
                receipt_out: absolute(payload, "receipt_out")?,
            };
            operator::integrate_pending(&cfg, &mut sink).map_err(msg)
        }
        "submit_main" => {
            let repo = required_path(payload, "repo")?;
            let raw_adopted: Vec<String> = payload["adopt_host_commits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let adopted_host_commits =
                susi_harness::git_governor::parse_adopted_oids(&raw_adopted).map_err(msg)?;
            let cfg = susi_harness::git_governor::SubmitMainConfig {
                repo,
                rev: field(payload, "rev").unwrap_or("HEAD").to_string(),
                remote: field(payload, "remote").unwrap_or("origin").to_string(),
                require_task_trailer: payload
                    .get("require_task_trailer")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                adopted_host_commits,
                dry_run: dry_run_flag,
                max_attempts: number(payload, "max_attempts").map_or(4, |n| n as usize),
            };
            operator::submit_main(&cfg, &mut sink).map_err(msg)
        }
        "promote_release" => {
            let repo = required_path(payload, "repo")?;
            let cfg = susi_harness::git_governor::ReleasePromotionConfig {
                repo,
                remote: field(payload, "remote").unwrap_or("origin").to_string(),
                fetch_remote: payload
                    .get("fetch_remote")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                target_release_name: field(payload, "name").map(str::to_string),
                candidate_rev: field(payload, "candidate").map(str::to_string),
                dry_run: dry_run_flag,
                receipt_out: absolute(payload, "receipt_out")?,
            };
            operator::promote_release(&cfg, &mut sink).map_err(msg)
        }
        other => Err(format!("unknown harness action `{other}`")),
    };
    with_notes_and_path(doc, notes, delegation_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_harness::executor::{ExecutorConfig, RunOutcome, Runner};
    use susi_harness::recursive::{CapabilityContract, ContainerRole, RECURSIVE_CONTRACT_SCHEMA};
    use susi_harness::supervisor::execute_recursive_tree_with;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-harness-ops-{tag}-{}-{}",
            std::process::id(),
            operator::now_unix_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dispatch_refuses_relative_paths_and_missing_arguments() {
        let refused = |payload: Value| dispatch(&payload).unwrap_err();
        assert!(refused(json!({})).contains("action"));
        assert!(refused(json!({"action": "nope"})).contains("unknown harness action"));
        assert!(refused(json!({"action": "status"})).contains("ledger"));
        assert!(refused(json!({"action": "status", "ledger": "rel.jsonl"})).contains("absolute"));
        assert!(refused(json!({"action": "plan"})).contains("repo"));
        assert!(refused(json!({"action": "models", "mode": "bogus"})).contains("unknown models"));
        assert!(
            refused(json!({"action": "models", "provider": "x"})).contains("verify or discover")
        );
        let repo = temp_dir("refuse");
        let repo_text = repo.display().to_string();
        assert!(refused(json!({"action": "author", "repo": repo_text})).contains("gateway"));
        assert!(
            refused(json!({"action": "run", "repo": repo_text, "plan": "p.json"}))
                .contains("absolute")
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn status_and_registry_answer_from_local_state() {
        let repo = temp_dir("status");
        let ledger = repo.join("ledger.jsonl");
        std::fs::write(&ledger, "").unwrap();
        let status = dispatch(&json!({"action": "status", "ledger": ledger})).unwrap();
        assert!(status.is_object());
        assert_eq!(
            status["delegation_path"],
            json!([
                "daemon-operator:root",
                "daemon-operator:daemon.harness.status"
            ])
        );
        let registry = dispatch(&json!({"action": "models"})).unwrap();
        assert!(registry["providers"].is_array());
        assert!(dispatch(&json!({"action": "runs"})).unwrap()["result"].is_array());
        let contracts = dispatch(&json!({"action": "recursive_contracts"})).unwrap();
        assert!(!contracts["contracts"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn run_options_resolve_cli_defaults_against_the_repository() {
        let repo = temp_dir("opts");
        let plan = repo.join("plan.json");
        let opts = run_options(&json!({
            "repo": repo, "plan": plan, "gateway": "http://127.0.0.1:9180/v1"
        }))
        .unwrap();
        assert_eq!(opts.catalogue, repo.join("deploy/harness/prices.json"));
        assert_eq!(opts.controls, repo.join("out/control.jsonl"));
        assert_eq!(opts.attempts, 3);
        assert!(opts.adapter_state.starts_with(&repo));
        // A plan that does not exist is refused before a thread starts.
        assert!(start_run(opts).unwrap_err().contains("load"));
        // A caller cannot choose the binary a socket run drives.
        let refused = run_options(&json!({
            "repo": repo, "plan": plan, "gateway": "http://127.0.0.1:9180/v1",
            "susi": repo.join("evil.exe"),
        }))
        .unwrap_err();
        assert!(refused.contains("susi cannot be chosen"), "{refused}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_panicking_worker_still_finalizes_its_record() {
        let mission = format!("panic-test-{}", operator::now_unix_ms());
        let scope = standalone_run_scope().unwrap();
        runs().lock().unwrap().insert(
            mission.clone(),
            RunRecord {
                state: PathBuf::from("state.json"),
                started_unix_ms: 0,
                notes: Vec::new(),
                delegation_path: scope_path_strings(&scope),
                scope,
                cancel: CancellationToken::new(),
                outcome: None,
            },
        );
        let thread_mission = mission.clone();
        let joined = std::thread::spawn(move || {
            run_worker(&thread_mission, || panic!("injected run failure"));
        })
        .join();
        assert!(joined.is_err(), "the injected panic propagates");
        let table = runs().lock().unwrap_or_else(|e| e.into_inner());
        let outcome = table[&mission].outcome.clone().expect("finalized");
        assert!(
            outcome["result"]["error"]
                .as_str()
                .unwrap()
                .contains("panicked")
        );
        drop(table);
        // A normal worker records its own outcome.
        run_worker(&mission, || (vec!["n".into()], json!({"result": "ok"})));
        let table = runs().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(table[&mission].outcome, Some(json!({"result": "ok"})));
        assert_eq!(table[&mission].notes, vec!["n".to_string()]);
    }

    #[test]
    fn models_mode_parses_all_supported_modes_and_rejects_unknown() {
        assert_eq!(models_mode(None).unwrap(), ModelsMode::Registry);
        assert_eq!(models_mode(Some("registry")).unwrap(), ModelsMode::Registry);
        assert_eq!(models_mode(Some("verify")).unwrap(), ModelsMode::Verify);
        assert_eq!(models_mode(Some("discover")).unwrap(), ModelsMode::Discover);
        assert_eq!(
            models_mode(Some("available")).unwrap(),
            ModelsMode::Available
        );
        assert!(models_mode(Some("invalid-mode")).is_err());
    }

    #[test]
    fn signed_delegation_envelope_attenuates_harness_ops_and_supports_revocation() {
        let repo = temp_dir("delegation");
        let ledger = repo.join("ledger.jsonl");
        std::fs::write(&ledger, "").unwrap();

        let authority = DelegationAuthority::from_secret(&[23u8; 32]);
        let root = DelegationScope::root(
            DelegationHop::new("ceo-session", "root").unwrap(),
            [PERM_HARNESS_READ.to_string(), PERM_HARNESS_RUN.to_string()],
            1_000,
            Some(60_000),
            DelegationLimits::default(),
        )
        .unwrap();
        let read_only_child = root
            .delegate(DelegationRequest {
                hop: DelegationHop::new("dot-driver", "inspect").unwrap(),
                permissions: [PERM_HARNESS_READ.to_string()].into_iter().collect(),
                budget_micros: 400,
                timeout_ms: 30_000,
            })
            .unwrap();
        let read_envelope = authority.issue(&read_only_child).unwrap();
        let read_env_id = read_envelope.id.clone().unwrap();

        // Read-only action succeeds under read_envelope and preserves full hop provenance.
        let ok_status = dispatch_with_authority(
            &json!({
                "action": "status",
                "subject": "opus-engine",
                "ledger": ledger,
                "budget_micros": 100,
                "delegation": read_envelope,
            }),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(
            ok_status["delegation_path"],
            json!([
                "ceo-session:root",
                "dot-driver:inspect",
                "opus-engine:daemon.harness.status"
            ])
        );

        // Permission amplification (`plan` requires `harness:plan`) is refused!
        let amp_err = dispatch_with_authority(
            &json!({
                "action": "plan",
                "subject": "opus-engine",
                "repo": repo,
                "out": repo.join("out.json"),
                "delegation": read_envelope,
            }),
            Some(&authority),
        )
        .unwrap_err();
        assert!(amp_err.contains("amplify permissions"), "{amp_err}");

        // Over-budget request against remaining envelope budget is refused!
        let budget_err = dispatch_with_authority(
            &json!({
                "action": "status",
                "subject": "opus-engine",
                "ledger": ledger,
                "budget_micros": 500,
                "delegation": read_envelope,
            }),
            Some(&authority),
        )
        .unwrap_err();
        assert!(budget_err.contains("exceeds"), "{budget_err}");

        // Revoking the envelope via `revoke_delegation` cancels any further presentations!
        let run_root_env = authority.issue(&root).unwrap();
        let rev = dispatch_with_authority(
            &json!({
                "action": "revoke_delegation",
                "subject": "ceo-admin",
                "id": read_env_id,
                "delegation": run_root_env,
            }),
            Some(&authority),
        )
        .unwrap();
        assert_eq!(rev["revoked"], json!(true));

        let cancelled_err = dispatch_with_authority(
            &json!({
                "action": "status",
                "subject": "opus-engine",
                "ledger": ledger,
                "delegation": read_envelope,
            }),
            Some(&authority),
        )
        .unwrap_err();
        assert!(cancelled_err.contains("cancelled"), "{cancelled_err}");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn preflight_recursive_delegation_enforces_sibling_budgets_cycles_and_isolated_execution() {
        struct StubRunner;
        impl Runner for StubRunner {
            fn run(&self, config: &ExecutorConfig) -> susi_error::EaiResult<RunOutcome> {
                Ok(RunOutcome::Succeeded {
                    final_text: format!("ok:{}:{}", config.model, config.task),
                    reported_cost_usd: Some(0.0),
                })
            }
        }

        let scratch = temp_dir("rec-bridge");
        let parent = DelegationScope::root(
            DelegationHop::new("master-brain", "root").unwrap(),
            [
                PERM_HARNESS_RUN.to_string(),
                "cap:wave_plan".to_string(),
                "cap:code_edit".to_string(),
            ],
            500,
            Some(30_000),
            DelegationLimits::default(),
        )
        .unwrap();

        // Two concurrent siblings asking for 300 + 300 micros out of a 500-micro parent fail!
        let overbooked = RecursiveTaskNode {
            task_key: "drv".to_string(),
            component_id: "susi-harness".to_string(),
            required_capabilities: vec!["wave_plan".to_string()],
            prompt: "drive".to_string(),
            grant: AuthorityGrant {
                allowed_scopes: vec!["crates".to_string()],
                allowed_capabilities: ["wave_plan", "code_edit"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                allow_network: false,
                allow_git_publish: false,
                max_depth: 3,
                budget_cap: MicroUsd::from_micros(500),
                timeout_ms: 20_000,
            },
            child_waves: vec![vec![
                RecursiveTaskNode {
                    task_key: "sib_a".to_string(),
                    component_id: "cli:claude".to_string(),
                    required_capabilities: vec!["code_edit".to_string()],
                    prompt: "a".to_string(),
                    grant: AuthorityGrant {
                        allowed_scopes: vec!["crates/susi-core".to_string()],
                        allowed_capabilities: ["code_edit"]
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                        allow_network: false,
                        allow_git_publish: false,
                        max_depth: 1,
                        budget_cap: MicroUsd::from_micros(300),
                        timeout_ms: 10_000,
                    },
                    child_waves: Vec::new(),
                },
                RecursiveTaskNode {
                    task_key: "sib_b".to_string(),
                    component_id: "cli:codex".to_string(),
                    required_capabilities: vec!["code_edit".to_string()],
                    prompt: "b".to_string(),
                    grant: AuthorityGrant {
                        allowed_scopes: vec!["crates/susi-harness".to_string()],
                        allowed_capabilities: ["code_edit"]
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                        allow_network: false,
                        allow_git_publish: false,
                        max_depth: 1,
                        budget_cap: MicroUsd::from_micros(300),
                        timeout_ms: 10_000,
                    },
                    child_waves: Vec::new(),
                },
            ]],
        };
        let cancel = CancellationToken::new();
        let over_err = preflight_recursive_delegation(&parent, &overbooked, &cancel).unwrap_err();
        assert!(over_err.contains("exceeds"), "{over_err}");
        // Refusal leaves parent budget intact!
        assert_eq!(parent.remaining_micros(), 500);

        // Well-budgeted tree (200 + 200 <= 500) passes preflight AND runs isolated sandboxes!
        let mut valid_tree = overbooked;
        valid_tree.child_waves[0][0].grant.budget_cap = MicroUsd::from_micros(200);
        valid_tree.child_waves[0][1].grant.budget_cap = MicroUsd::from_micros(200);

        let tree_scope = preflight_recursive_delegation(&parent, &valid_tree, &cancel).unwrap();
        let contracts = vec![
            CapabilityContract {
                contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
                component_id: "susi-harness".to_string(),
                role: ContainerRole::DriverAndEngine,
                capabilities: ["wave_plan", "code_edit"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                verified_available: true,
            },
            CapabilityContract {
                contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
                component_id: "cli:claude".to_string(),
                role: ContainerRole::DriverAndEngine,
                capabilities: ["code_edit"].into_iter().map(str::to_string).collect(),
                verified_available: true,
            },
            CapabilityContract {
                contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
                component_id: "cli:codex".to_string(),
                role: ContainerRole::DriverAndEngine,
                capabilities: ["code_edit"].into_iter().map(str::to_string).collect(),
                verified_available: true,
            },
        ];
        let run = RecursiveTreeRun {
            sandbox_root: &scratch,
            gateway_url: "http://127.0.0.1:9185",
            default_model: "deepseek-flash",
            root_run_id: "wave_ok",
            node: &valid_tree,
            base_port_offset: 120,
            cancel: &cancel,
        };
        let receipt = execute_recursive_tree_with(&run, StubRunner, &contracts).unwrap();
        assert!(receipt.verify_integrity());
        assert_eq!(receipt.children.len(), 2);
        assert_ne!(
            receipt.children[0].sandbox.port_offset,
            receipt.children[1].sandbox.port_offset
        );
        drop(tree_scope);
        // Unspent budget returned cleanly to `parent`!
        assert_eq!(parent.remaining_micros(), 500);

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn git_governor_actions_require_git_publish_permission_when_not_dry_run() {
        let scratch = temp_dir("git-gov-perm");
        let authority = DelegationAuthority::from_secret(&[41u8; 32]);
        let no_publish_scope = DelegationScope::root(
            DelegationHop::new("agent-worker", "root").unwrap(),
            [PERM_HARNESS_RUN.to_string()],
            0,
            Some(30_000),
            DelegationLimits::default(),
        )
        .unwrap();
        let no_publish_env = authority.issue(&no_publish_scope).unwrap();

        for action in ["integrate_pending", "submit_main", "promote_release"] {
            let err = dispatch_with_authority(
                &json!({
                    "action": action,
                    "subject": "agent-worker",
                    "repo": scratch,
                    "dry_run": false,
                    "delegation": no_publish_env,
                }),
                Some(&authority),
            )
            .unwrap_err();
            assert!(
                err.contains("amplify permissions") && err.contains("git:publish"),
                "{action} must refuse live publish without git:publish, got: {err}"
            );
        }

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
