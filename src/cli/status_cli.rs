//! `susi status` — one deterministic surface for what the substrate is
//! doing right now: missions in flight, the scheduled queue, brain state
//! and the model it would choose, recorded spend, and pending approvals.
//!
//! Every line is read straight from the persisted stores and live process
//! checks — the mission dispatch dir, `scheduled-missions.json`, brain
//! evidence, the usage ledger, and the file-backed IPC broker. Nothing
//! here asks an LLM to narrate state: the previous implementation sent a
//! bare "status" prompt through the swarm, which could report whatever
//! sounded plausible (Mandate 2 — measured state or no claim).

use std::path::{Path, PathBuf};

use susi_core::broker::IpcBroker;
use susi_daemon::{scheduled_missions::ScheduleStore, SusiDaemon};
use susi_gemi::engines::{brain, brain::TaskClass, cost, routing::InferenceRouter};
use susi_gemi::usage_accounting::UsageLedger;

/// A mission dispatched to the ingress dir, not yet claimed by a worker.
#[derive(Debug)]
pub(crate) struct MissionInFlight {
    pub id: String,
    pub prompt_head: String,
    pub dispatched_unix: u64,
}

/// Snapshot of the scheduled-mission queue.
#[derive(Debug, Default)]
pub(crate) struct QueueSection {
    pub total: usize,
    pub paused: usize,
    pub due_now: usize,
    /// Seconds until the earliest unpaused mission is due (None if every
    /// unpaused mission is already due or the queue is empty).
    pub next_due_in_secs: Option<u64>,
    /// Worst consecutive-failure streak across the queue.
    pub max_failures: u32,
}

/// Evidence store + ranking digest.
#[derive(Debug, Default)]
pub(crate) struct BrainSection {
    pub providers_with_evidence: Vec<String>,
    /// (task class, leading provider) for each class with a leader.
    pub leaders: Vec<(String, String)>,
    /// Providers currently marked unfit (`provider|class` keys).
    pub unfit: Vec<String>,
    pub failure_streaks: usize,
}

/// Routing preference and availability digest.
#[derive(Debug, Default)]
pub(crate) struct RoutingSection {
    pub preferred_cloud: Option<String>,
    pub policy_override: Option<String>,
    pub auto_switched_from: Option<String>,
    /// (provider, cooldown seconds remaining).
    pub cooled: Vec<(String, u64)>,
    /// The local floor model that would serve a request now.
    pub chosen_local_model: Option<String>,
    pub budget_label: &'static str,
}

/// Measured token spend from the usage ledger (USD requires a price the
/// ledger never records, so this reports the always-measurable side:
/// calls, successes, tokens — Mandate 2: no invented dollar figures).
#[derive(Debug, Default)]
pub(crate) struct SpendSection {
    pub calls: u64,
    pub successes: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// per-provider (calls, successes) sorted by call volume.
    pub by_provider: Vec<(String, u64, u64)>,
    /// The measured cost of observation itself (VC-202-018): bytes the
    /// audit/trace tiers persisted plus the write overhead, attributed
    /// per agent like any other spend.
    pub observation_bytes: u64,
    pub observation_ms: u64,
    /// agent → bytes of observation that agent's actions wrote.
    pub observation_by_agent: Vec<(String, u64)>,
}

/// One pending approval request seen on the shared broker.
#[derive(Debug)]
pub(crate) struct PendingApproval {
    pub id: String,
    pub requester: String,
    pub resource: String,
    pub action: String,
}

/// A durable workspace DAG mission a restart may resume — the explicit
/// resume point `susi status` surfaces (T-DEEPSEEK-93, VC-202-011).
#[derive(Debug)]
pub(crate) struct ResumableMission {
    pub id: String,
    /// Nodes still unfinished (pending or crash-interrupted running).
    pub resumable_nodes: Vec<String>,
    /// How many restarts already folded interrupted work on this mission.
    pub interruptions: usize,
}

/// A terminally failed mission that still has a snapshot to roll back
/// to — the known-good point an operator can restore (T-DEEPSEEK-94).
#[derive(Debug)]
pub(crate) struct RollbackReady {
    pub id: String,
    pub snapshots: usize,
}

/// The whole surface, collected once so `render` is a pure function of it.
#[derive(Debug)]
pub(crate) struct UnifiedStatus {
    pub workspace: PathBuf,
    pub daemon_pid: Option<u32>,
    pub missions: Vec<MissionInFlight>,
    pub resumable: Vec<ResumableMission>,
    pub rollback_ready: Vec<RollbackReady>,
    pub queue: QueueSection,
    pub brain: BrainSection,
    pub routing: RoutingSection,
    pub spend: SpendSection,
    pub approvals: Vec<PendingApproval>,
}

/// Canonical usage-ledger location: sibling of `brain_evidence.json`, the
/// file `UsageLedger`'s own doc names as its home.
fn usage_ledger_path(global_dir: &Path) -> PathBuf {
    global_dir.join("usage.json")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read every persisted store once and fold it into a `UnifiedStatus`.
/// `global_dir` is the substrate config dir (brain evidence, usage ledger,
/// identity); `substrate_home` is the daemon's data dir (mission dispatch
/// dir, scheduled-missions.json — `runtime_admin` binds there).
/// `now` is a parameter so tests pin the clock instead of racing `due`.
pub(crate) fn collect(
    cwd: &Path,
    global_dir: &Path,
    substrate_home: &Path,
    now: u64,
) -> UnifiedStatus {
    let missions = read_missions(substrate_home);
    let resumable = read_resumable_missions(cwd);
    let rollback_ready = read_rollback_ready(cwd);
    let queue = read_queue(substrate_home, now);
    let brain = read_brain(now);
    let routing = read_routing(cwd, now);
    let spend = read_spend(global_dir);
    let approvals = read_approvals();
    UnifiedStatus {
        workspace: cwd.to_path_buf(),
        daemon_pid: SusiDaemon::check_status(cwd, global_dir),
        missions,
        resumable,
        rollback_ready,
        queue,
        brain,
        routing,
        spend,
        approvals,
    }
}

fn read_missions(substrate_home: &Path) -> Vec<MissionInFlight> {
    let dir = substrate_home.join("missions");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
        let prompt_head: String = v
            .get("prompt")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        let dispatched_unix = v
            .get("dispatched_unix")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        out.push(MissionInFlight {
            id,
            prompt_head,
            dispatched_unix,
        });
    }
    out.sort_by_key(|m| m.dispatched_unix);
    out
}

/// Workspace DAG missions a restart may resume — the same
/// `<workspace>/.susi/missions/*.json` records `mission_persist` writes.
/// A mission with a recorded `failed` node is terminal, not resumable;
/// a mission left `running` was crash-interrupted and folds back on
/// resume (T-DEEPSEEK-93).
fn read_resumable_missions(workspace: &Path) -> Vec<ResumableMission> {
    let dir = workspace.join(".susi").join("missions");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(nodes) = v.get("nodes").and_then(|n| n.as_object()) else {
            continue;
        };
        let failed = nodes
            .values()
            .any(|n| n.get("state").and_then(|s| s.as_str()) == Some("failed"));
        if failed {
            continue;
        }
        let resumable_nodes: Vec<String> = nodes
            .values()
            .filter(|n| {
                matches!(
                    n.get("state").and_then(|s| s.as_str()),
                    Some("pending") | Some("running")
                )
            })
            .filter_map(|n| n.get("id").and_then(|i| i.as_str()).map(str::to_string))
            .collect();
        if resumable_nodes.is_empty() {
            continue;
        }
        let id = v
            .get("mission_id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
        let interruptions = v
            .get("interruptions")
            .and_then(|i| i.as_array())
            .map_or(0, |a| a.len());
        out.push(ResumableMission {
            id,
            resumable_nodes,
            interruptions,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Terminally failed missions that still hold a snapshot — the
/// known-good point `gawd.mission.rollback` restores (T-DEEPSEEK-94).
/// Same records `read_resumable_missions` scans, plus the
/// `.susi/mission-snapshots/<id>/` dirs `PersistedMission::snapshot`
/// writes.
fn read_rollback_ready(workspace: &Path) -> Vec<RollbackReady> {
    let dir = workspace.join(".susi").join("missions");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(nodes) = v.get("nodes").and_then(|n| n.as_object()) else {
            continue;
        };
        let failed = nodes
            .values()
            .any(|n| n.get("state").and_then(|s| s.as_str()) == Some("failed"));
        if !failed {
            continue;
        }
        let id = v
            .get("mission_id")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
        let snap_dir = workspace.join(".susi").join("mission-snapshots").join(&id);
        let snapshots = std::fs::read_dir(&snap_dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                    .count()
            })
            .unwrap_or(0);
        if snapshots == 0 {
            continue;
        }
        out.push(RollbackReady { id, snapshots });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn read_queue(substrate_home: &Path, now: u64) -> QueueSection {
    let path = ScheduleStore::path_in(substrate_home);
    let Ok(store) = ScheduleStore::load(&path) else {
        return QueueSection::default();
    };
    let mut q = QueueSection {
        total: store.missions.len(),
        ..QueueSection::default()
    };
    let mut next_due: Option<u64> = None;
    for m in &store.missions {
        if m.paused {
            q.paused += 1;
            continue;
        }
        q.max_failures = q.max_failures.max(m.consecutive_failures);
        let due_at = m.due_at();
        if m.is_due(now) {
            q.due_now += 1;
        } else {
            let wait = due_at.saturating_sub(now);
            next_due = Some(next_due.map_or(wait, |n: u64| n.min(wait)));
        }
    }
    q.next_due_in_secs = next_due;
    q
}

fn read_brain(now: u64) -> BrainSection {
    let store = brain::load();
    let providers = store.providers();
    let leaders = TaskClass::ALL
        .iter()
        .filter_map(|c| {
            store
                .rank(&providers, *c)
                .into_iter()
                .find(|r| !r.unfit && !store.is_unfit(&r.provider, *c, now))
                .map(|r| (c.label().to_string(), r.provider))
        })
        .collect();
    let mut unfit = Vec::new();
    for p in &providers {
        for c in TaskClass::ALL {
            if store.is_unfit(p, c, now) {
                unfit.push(format!("{p}|{}", c.label()));
            }
        }
    }
    BrainSection {
        providers_with_evidence: providers,
        leaders,
        unfit,
        failure_streaks: store.failure_streaks().len(),
    }
}

fn read_routing(cwd: &Path, now: u64) -> RoutingSection {
    let pref = InferenceRouter::load_preference();
    let cooled: Vec<(String, u64)> = InferenceRouter::cooled_providers()
        .into_iter()
        .map(|c| (c.provider, c.until_unix.saturating_sub(now)))
        .collect();
    let chosen_local_model =
        susi_gemi::models::ModelManager::identify_best_suited_local_model(cwd, None)
            .map(|m| m.name().to_string());
    RoutingSection {
        preferred_cloud: pref.preferred_cloud,
        policy_override: pref.policy_override,
        auto_switched_from: pref.auto_switched_from,
        cooled,
        chosen_local_model,
        budget_label: cost::Budget::from_env().label(),
    }
}

fn read_spend(global_dir: &Path) -> SpendSection {
    let path = usage_ledger_path(global_dir);
    let Ok(ledger) = UsageLedger::load(&path) else {
        return SpendSection::default();
    };
    // `UsageLedger` keeps its records private and `summary` is keyed per
    // provider, so enumerate providers from the persisted JSON (distinct
    // `provider` keys in `records`) and aggregate through the real API.
    let providers = ledger_providers(&path);
    let mut s = SpendSection::default();
    for provider in providers {
        let sum = ledger.summary(&provider, "", None);
        s.calls += sum.calls;
        s.successes += sum.successes;
        s.prompt_tokens += sum.prompt_tokens;
        s.completion_tokens += sum.completion_tokens;
        s.by_provider.push((provider, sum.calls, sum.successes));
    }
    s.by_provider
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    // The cost of observing all of the above, attributed like any other
    // spend — the observation ledger sits beside `usage.json`, appended
    // one JSON record per write by the audit/trace tiers (same read-the-
    // records convention as `ledger_providers` above).
    let journal = global_dir.join("observation-cost.jsonl");
    if let Ok(text) = std::fs::read_to_string(&journal) {
        let mut by_agent: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let bytes = record.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
            s.observation_bytes += bytes;
            s.observation_ms += record
                .get("elapsed_ns")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                / 1_000_000;
            if let Some(agent) = record.get("agent").and_then(|a| a.as_str()) {
                *by_agent.entry(agent.to_string()).or_default() += bytes;
            }
        }
        s.observation_by_agent = by_agent.into_iter().collect();
        s.observation_by_agent
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    }
    s
}

/// Distinct `provider` keys in the ledger's `records` array — the same
/// file `UsageLedger::load` reads.
fn ledger_providers(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut providers: Vec<String> = v
        .get("records")
        .and_then(|r| r.as_array())
        .map(|records| {
            records
                .iter()
                .filter_map(|r| {
                    r.get("provider")
                        .and_then(|p| p.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    providers.sort();
    providers.dedup();
    providers
}

fn read_approvals() -> Vec<PendingApproval> {
    IpcBroker::global()
        .pending_requests(None)
        .into_iter()
        .map(|r| PendingApproval {
            id: r.id,
            requester: r.requester,
            resource: r.scope.resource,
            action: r.scope.action,
        })
        .collect()
}

/// Render the collected snapshot as the operator-facing report.
pub(crate) fn render(s: &UnifiedStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("susi status — {}\n", s.workspace.display()));
    match s.daemon_pid {
        Some(pid) => out.push_str(&format!("daemon: running (pid {pid})\n")),
        None => out.push_str("daemon: not running\n"),
    }

    out.push_str(&format!("missions in flight: {}\n", s.missions.len()));
    for m in &s.missions {
        out.push_str(&format!(
            "  {} — {} (dispatched @{})\n",
            m.id, m.prompt_head, m.dispatched_unix
        ));
    }
    out.push_str(&format!("resumable DAG missions: {}\n", s.resumable.len()));
    for m in &s.resumable {
        out.push_str(&format!(
            "  {} — resume nodes: {} ({} interruption(s) recorded)\n",
            m.id,
            m.resumable_nodes.join(","),
            m.interruptions
        ));
    }
    out.push_str(&format!(
        "failed missions with snapshots: {}\n",
        s.rollback_ready.len()
    ));
    for m in &s.rollback_ready {
        out.push_str(&format!(
            "  {} — {} snapshot(s); rollback via gawd.mission.rollback\n",
            m.id, m.snapshots
        ));
    }

    let q = &s.queue;
    out.push_str(&format!(
        "scheduled queue: {} (paused {}, due now {}, max failure streak {})\n",
        q.total, q.paused, q.due_now, q.max_failures
    ));
    if let Some(wait) = q.next_due_in_secs {
        out.push_str(&format!("  next run in {wait}s\n"));
    }

    let b = &s.brain;
    out.push_str(&format!(
        "brain: {} provider(s) with evidence, {} failure streak(s)\n",
        b.providers_with_evidence.len(),
        b.failure_streaks
    ));
    for (class, provider) in &b.leaders {
        out.push_str(&format!("  {class}: {provider}\n"));
    }
    if !b.unfit.is_empty() {
        out.push_str(&format!("  unfit: {}\n", b.unfit.join(", ")));
    }

    let r = &s.routing;
    out.push_str(&format!("budget: {}\n", r.budget_label));
    match &r.chosen_local_model {
        Some(m) => out.push_str(&format!("local model: {m}\n")),
        None => out.push_str("local model: none selected\n"),
    }
    match &r.preferred_cloud {
        Some(p) => out.push_str(&format!("preferred cloud: {p}\n")),
        None => out.push_str("preferred cloud: none\n"),
    }
    if let Some(pol) = &r.policy_override {
        out.push_str(&format!("routing policy: {pol}\n"));
    }
    if let Some(prev) = &r.auto_switched_from {
        out.push_str(&format!("auto-switched away from: {prev}\n"));
    }
    if !r.cooled.is_empty() {
        let cooled: Vec<String> = r
            .cooled
            .iter()
            .map(|(p, secs)| format!("{p} ({secs}s)"))
            .collect();
        out.push_str(&format!("cooling down: {}\n", cooled.join(", ")));
    }

    let sp = &s.spend;
    if sp.calls == 0 {
        out.push_str("spend: no usage recorded\n");
    } else {
        out.push_str(&format!(
            "spend: {} calls ({} ok), {} prompt + {} completion tokens\n",
            sp.calls, sp.successes, sp.prompt_tokens, sp.completion_tokens
        ));
        for (p, calls, ok) in &sp.by_provider {
            out.push_str(&format!("  {p}: {calls} calls, {ok} ok\n"));
        }
    }
    if sp.observation_bytes > 0 {
        out.push_str(&format!(
            "observation: {} bytes written, {} ms overhead\n",
            sp.observation_bytes, sp.observation_ms
        ));
        for (agent, bytes) in &sp.observation_by_agent {
            out.push_str(&format!("  {agent}: {bytes} bytes\n"));
        }
    }

    out.push_str(&format!("pending approvals: {}\n", s.approvals.len()));
    for a in &s.approvals {
        out.push_str(&format!(
            "  {} asks {}:{} ({})\n",
            a.requester, a.resource, a.action, a.id
        ));
    }
    out
}

/// The `susi status` command body.
pub(crate) fn run(cwd: &Path, global_dir: &Path) {
    let status = collect(
        cwd,
        global_dir,
        &susi_paths::SusiDirs::substrate_home(),
        now_unix(),
    );
    print!("{}", render(&status));
}
