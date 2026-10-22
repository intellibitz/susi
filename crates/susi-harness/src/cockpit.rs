//! The cockpit.
//!
//! A status and control surface over what the harness already writes: the
//! ledger (spend per mission and per model), the supervisor's state (which
//! tasks landed), the event feed (what happened) and the control log (what the
//! operator asked). It recomputes on every request, so there is no second
//! source of truth to drift; the panel receives a snapshot over SSE once a
//! second, with a slow HTTP poll as a fallback.
//!
//! The panel is a plain HTML page with no build step and no external assets, so
//! any harness desktop can embed it in an iframe. [`router`] is the whole
//! contract: `GET /` (panel), `GET /api/status` (snapshot), `GET /api/events`
//! (SSE) and `POST /api/command` (operator control).

use crate::api::{ControlAck, ControlCommand};
use crate::budget::Budget;
use crate::catalog::PriceCatalogue;
use crate::control::{self, ControlRecord};
use crate::event::{append_event, read_events, Event, EventKind};
use crate::ledger::{read_entries, LedgerEntry, Outcome};
use crate::mission_control::GraphView;
use crate::money::MicroUsd;
use crate::pricing::ModelPrice;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the cockpit reads from.
#[derive(Clone)]
pub struct Cockpit {
    /// Ledger the gateway writes.
    pub ledger_path: PathBuf,
    /// Event feed the supervisor or scheduler writes.
    pub events_path: PathBuf,
    /// Supervisor resume state, when the cockpit watches a plan.
    pub state_path: Option<PathBuf>,
    /// Plan the supervisor is running, when there is one.
    pub plan_path: Option<PathBuf>,
    /// Control log the operator's commands are appended to.
    pub controls_path: Option<PathBuf>,
    /// Price catalogue, for showing each worker's rate.
    pub catalogue_path: Option<PathBuf>,
    /// Agent-graph bus file, when the operator named one; overrides the
    /// environment and the `graph.jsonl` beside the ledger.
    pub graph_path: Option<PathBuf>,
    /// Mission cap to display, when the operator started the cockpit with one.
    pub budget: Option<Budget>,
    /// Mission to focus; otherwise the plan's mission, otherwise the newest.
    pub mission: Option<String>,
}

/// One mission, in the operator's terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionView {
    /// Mission id.
    pub mission: String,
    /// planned, running, paused, aborted, done, failed or reported.
    pub state: String,
    /// Tasks the ledger shows made calls (distinct task ids).
    pub tasks: usize,
    /// Tasks the plan contains, when a plan is watched.
    pub planned: usize,
    /// Tasks the supervisor recorded as done.
    pub done: usize,
    /// Tasks the supervisor recorded as failed.
    pub failed: usize,
    /// Priced calls.
    pub calls: usize,
    /// Total spend in micro-USD.
    pub cost_micro_usd: u64,
    /// The mission cap, when it has one.
    pub budget_micro_usd: Option<u64>,
    /// Models that made calls, ordered by name.
    pub workers: Vec<String>,
    /// Last event for this mission, milliseconds since the Unix epoch.
    pub updated_unix_ms: u64,
    /// Distinct tasks that made calls.
    #[serde(skip)]
    worker_set: BTreeSet<String>,
    /// The most recent event kind for this mission.
    #[serde(skip)]
    last_kind: Option<EventKind>,
}

/// One model's record across every mission in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkerView {
    /// Model id.
    pub model: String,
    /// Providers that served it.
    pub providers: Vec<String>,
    /// Priced calls.
    pub calls: usize,
    /// Calls that were not rejected or failed.
    pub successes: usize,
    /// Calls the provider or acceptance failed.
    pub failures: usize,
    /// Total spend in micro-USD.
    pub cost_micro_usd: u64,
    /// The model's current price, when the catalogue knows it.
    pub price: Option<ModelPrice>,
}

/// A decision the supervisor is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionView {
    /// When it was requested.
    pub at_unix_ms: u64,
    /// What the operator must decide.
    pub summary: String,
    /// Whether a later event resolved it.
    pub resolved: bool,
}

/// One status snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// When this snapshot was built.
    pub generated_unix_ms: u64,
    /// Total priced spend in micro-USD.
    pub total_cost_micro_usd: u64,
    /// The focused mission's cap, when it has one.
    pub budget_micro_usd: Option<u64>,
    /// What is left of the focused mission's cap.
    pub budget_remaining_micro_usd: Option<u64>,
    /// Per-mission views, ordered by mission id.
    pub missions: Vec<MissionView>,
    /// Per-model views, ordered by model id.
    pub workers: Vec<WorkerView>,
    /// Decisions the supervisor requested, newest last.
    pub decisions: Vec<DecisionView>,
    /// The last operator commands, newest last.
    pub controls: Vec<ControlRecord>,
    /// Actionable warnings, in the operator's language.
    pub warnings: Vec<String>,
    /// The most recent events, oldest first.
    pub events: Vec<Event>,
    /// The agent graph's handoffs, when a bus is available.
    pub graph: GraphView,
    /// Spend per graph role in the focused mission, ordered by role.
    pub role_spend: Vec<RoleSpend>,
}

/// One graph role's spend in the focused mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleSpend {
    /// The role, as the agent graph names it.
    pub role: String,
    /// Ledger calls the role made in the focused mission.
    pub calls: usize,
    /// Spend in micro-USD.
    pub cost_micro_usd: u64,
}

impl Cockpit {
    /// Read the ledger, feed, state, plan and controls and build a snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let entries = read_entries(&self.ledger_path).unwrap_or_default();
        let mut events = read_events(&self.events_path).unwrap_or_default();
        let now = now_ms();
        let mut missions: BTreeMap<String, MissionView> = BTreeMap::new();
        let mut workers: BTreeMap<String, WorkerView> = BTreeMap::new();
        let mut total = MicroUsd::ZERO;

        for entry in &entries {
            total = total
                .checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX));
            let view = missions
                .entry(entry.mission.clone())
                .or_insert_with(|| MissionView::new(entry.mission.clone()));
            view.calls += 1;
            view.cost_micro_usd = view.cost_micro_usd.saturating_add(entry.cost.micros());
            view.worker_set.insert(entry.task.clone());
            let worker = workers
                .entry(entry.model.clone())
                .or_insert_with(|| WorkerView::new(entry.model.clone()));
            worker.calls += 1;
            worker.cost_micro_usd = worker.cost_micro_usd.saturating_add(entry.cost.micros());
            if !worker.providers.contains(&entry.provider) {
                worker.providers.push(entry.provider.clone());
            }
            match entry.outcome {
                Outcome::ModelFailed { .. } | Outcome::Rejected { .. } => worker.failures += 1,
                Outcome::Accepted | Outcome::Completed | Outcome::Cancelled => {
                    worker.successes += 1;
                }
            }
        }

        let plan = self.plan_path.as_deref().and_then(read_json);
        let plan_mission = plan
            .as_ref()
            .and_then(|plan| plan.get("mission"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let plan_tasks: Vec<String> = plan
            .as_ref()
            .and_then(|plan| plan.get("tasks"))
            .and_then(Value::as_array)
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(|task| task.get("key").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let progress = self
            .state_path
            .as_deref()
            .map(read_progress)
            .unwrap_or_default();

        for event in &events {
            let Some(mission) = &event.mission else {
                continue;
            };
            let view = missions
                .entry(mission.clone())
                .or_insert_with(|| MissionView::new(mission.clone()));
            view.updated_unix_ms = view.updated_unix_ms.max(event.at_unix_ms);
            view.last_kind = Some(event.kind);
        }

        if let Some(mission) = &plan_mission {
            let view = missions
                .entry(mission.clone())
                .or_insert_with(|| MissionView::new(mission.clone()));
            view.planned = plan_tasks.len();
            for key in &plan_tasks {
                match progress.get(key).map(|record| record.status.as_str()) {
                    Some("done") => view.done += 1,
                    Some("failed") => view.failed += 1,
                    _ => {}
                }
            }
        }

        let focus = self.focus(plan_mission.as_deref());
        let mut warnings = Vec::new();
        for view in missions.values_mut() {
            view.state = derive_state(view);
            view.tasks = view.worker_set.len();
            view.workers = view.worker_names();
            if let Some(budget) = self.budget {
                if Some(view.mission.as_str()) == focus.as_deref() {
                    view.budget_micro_usd = Some(budget.limit.micros());
                }
            }
            if view.failed > 0 {
                warnings.push(format!(
                    "{}: {} task(s) failed and are not landed",
                    view.mission, view.failed
                ));
            }
            match view.state.as_str() {
                "paused" => warnings.push(format!("{} is paused by the operator", view.mission)),
                "aborted" => warnings.push(format!("{} was aborted", view.mission)),
                "running" if now.saturating_sub(view.updated_unix_ms) > 15 * 60 * 1000 => {
                    warnings.push(format!(
                        "{}: no activity for {} minute(s)",
                        view.mission,
                        now.saturating_sub(view.updated_unix_ms) / 60_000
                    ));
                }
                _ => {}
            }
            if let Some(limit) = view.budget_micro_usd {
                let spent = MicroUsd::from_micros(view.cost_micro_usd);
                if limit > 0 && view.cost_micro_usd.saturating_mul(5) >= limit.saturating_mul(4) {
                    warnings.push(format!(
                        "{}: spend {} is at {}% of the {} cap",
                        view.mission,
                        spent,
                        view.cost_micro_usd.saturating_mul(100) / limit.max(1),
                        MicroUsd::from_micros(limit)
                    ));
                }
            }
            if view.planned > 0 && view.state != "running" && view.done + view.failed < view.planned
            {
                warnings.push(format!(
                    "{}: {} task(s) were never started",
                    view.mission,
                    view.planned - view.done - view.failed
                ));
            }
        }

        if let Some(catalogue_path) = &self.catalogue_path {
            if let Ok(catalogue) = PriceCatalogue::load(catalogue_path) {
                for worker in workers.values_mut() {
                    worker.price = catalogue.price(&worker.model);
                }
            }
        }

        let focus_spend = focus
            .as_ref()
            .and_then(|mission| missions.get(mission))
            .map_or(0, |view| view.cost_micro_usd);
        let budget_micro_usd = self.budget.map(|budget| budget.limit.micros());
        let budget_remaining_micro_usd =
            budget_micro_usd.map(|limit| limit.saturating_sub(focus_spend));

        let decisions = decisions(&events);
        let controls = self
            .controls_path
            .as_deref()
            .map(control::read_records)
            .unwrap_or_default();
        let mut controls = controls;
        if controls.len() > 20 {
            controls = controls.split_off(controls.len() - 20);
        }
        if events.len() > 50 {
            events = events.split_off(events.len() - 50);
        }
        let graph = graph_for(self);
        let role_spend = spend_by_role(&entries, focus.as_deref(), &graph.roles);

        Snapshot {
            generated_unix_ms: now,
            total_cost_micro_usd: total.micros(),
            budget_micro_usd,
            budget_remaining_micro_usd,
            missions: missions.into_values().collect(),
            workers: workers.into_values().collect(),
            decisions,
            controls,
            warnings,
            events,
            graph,
            role_spend,
        }
    }

    fn focus(&self, plan_mission: Option<&str>) -> Option<String> {
        self.mission
            .clone()
            .or_else(|| plan_mission.map(str::to_string))
            .or_else(|| {
                read_entries(&self.ledger_path)
                    .ok()
                    .and_then(|entries| entries.last().map(|entry| entry.mission.clone()))
            })
    }
}

/// Where the cockpit reads the agent-graph bus from.
///
/// `graph_path` names it first; then `SUSI_HARNESS_GRAPH`; otherwise a
/// `graph.jsonl` beside the ledger is used, which is where a mission's runner
/// writes it. No bus is not an error: the snapshot simply has an empty graph.
fn graph_for(cockpit: &Cockpit) -> GraphView {
    if let Some(named) = &cockpit.graph_path {
        return GraphView::load(named);
    }
    if let Ok(named) = std::env::var("SUSI_HARNESS_GRAPH") {
        let named = named.trim();
        if !named.is_empty() {
            return GraphView::load(Path::new(named));
        }
    }
    let beside_ledger = cockpit.ledger_path.with_file_name("graph.jsonl");
    if beside_ledger.exists() {
        return GraphView::load(&beside_ledger);
    }
    GraphView::default()
}

/// Sum the focused mission's ledger calls per graph role, ordered by role.
///
/// A role the graph knows but the ledger never paid for keeps a zero row, so
/// the panel can show the whole team; ledger tasks that are not graph roles
/// (a supervisor key, say) are not attributed to an agent.
fn spend_by_role(entries: &[LedgerEntry], focus: Option<&str>, roles: &[String]) -> Vec<RoleSpend> {
    let Some(focus) = focus else {
        return Vec::new();
    };
    let mut spend: BTreeMap<&str, (usize, u64)> = roles
        .iter()
        .map(|role| (role.as_str(), (0usize, 0u64)))
        .collect();
    for entry in entries {
        if entry.mission != focus {
            continue;
        }
        if let Some((calls, cost)) = spend.get_mut(entry.task.as_str()) {
            *calls += 1;
            *cost = cost.saturating_add(entry.cost.micros());
        }
    }
    spend
        .into_iter()
        .map(|(role, (calls, cost_micro_usd))| RoleSpend {
            role: role.to_string(),
            calls,
            cost_micro_usd,
        })
        .collect()
}

impl MissionView {
    fn new(mission: String) -> Self {
        Self {
            mission,
            state: "reported".into(),
            tasks: 0,
            planned: 0,
            done: 0,
            failed: 0,
            calls: 0,
            cost_micro_usd: 0,
            budget_micro_usd: None,
            workers: Vec::new(),
            updated_unix_ms: 0,
            worker_set: BTreeSet::new(),
            last_kind: None,
        }
    }

    fn worker_names(&self) -> Vec<String> {
        self.worker_set.iter().cloned().collect()
    }
}

impl WorkerView {
    fn new(model: String) -> Self {
        Self {
            model,
            providers: Vec::new(),
            calls: 0,
            successes: 0,
            failures: 0,
            cost_micro_usd: 0,
            price: None,
        }
    }
}

fn derive_state(view: &MissionView) -> String {
    match view.last_kind {
        Some(EventKind::MissionAborted) => return "aborted".into(),
        Some(EventKind::MissionPaused) => return "paused".into(),
        _ => {}
    }
    if view.planned > 0 && view.done >= view.planned {
        return "done".into();
    }
    if view.failed > 0 && view.last_kind == Some(EventKind::MissionFinished) {
        return "failed".into();
    }
    match view.last_kind {
        Some(
            EventKind::MissionStarted
            | EventKind::TaskDispatched
            | EventKind::TaskFinished
            | EventKind::ModelEscalated
            | EventKind::BudgetWarning,
        ) => "running".into(),
        Some(EventKind::MissionFinished) => "done".into(),
        Some(EventKind::MissionStuck) => "failed".into(),
        _ => "reported".into(),
    }
}

fn decisions(events: &[Event]) -> Vec<DecisionView> {
    let mut decisions = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.kind == EventKind::DecisionRequested)
    {
        let resolved = events.iter().any(|later| {
            later.kind == EventKind::DecisionResolved
                && (later.summary.contains(&event.summary)
                    || event.summary.contains(&later.summary))
        });
        decisions.push(DecisionView {
            at_unix_ms: event.at_unix_ms,
            summary: event.summary.clone(),
            resolved,
        });
    }
    if decisions.len() > 10 {
        decisions = decisions.split_off(decisions.len() - 10);
    }
    decisions
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Progress {
    status: String,
    reason: String,
}

fn read_progress(path: &Path) -> BTreeMap<String, Progress> {
    let Some(document) = read_json(path) else {
        return BTreeMap::new();
    };
    let mut progress = BTreeMap::new();
    if let Some(records) = document.get("records").and_then(Value::as_object) {
        for (key, record) in records {
            progress.insert(
                key.clone(),
                Progress {
                    status: record
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    reason: record
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
            );
        }
    }
    progress
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Build the cockpit router.
pub fn router(cockpit: Arc<Cockpit>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/command", post(command))
        .route("/health", get(health))
        .with_state(cockpit)
}

/// Serve the cockpit on an already-bound listener.
///
/// # Errors
///
/// Returns the listener's I/O error when the server stops.
pub async fn serve(
    listener: tokio::net::TcpListener,
    cockpit: Arc<Cockpit>,
) -> std::io::Result<()> {
    axum::serve(listener, router(cockpit)).await
}

async fn index() -> Html<&'static str> {
    Html(include_str!("panel.html"))
}

async fn health() -> &'static str {
    "ok"
}

async fn status(State(cockpit): State<Arc<Cockpit>>) -> Json<Snapshot> {
    Json(cockpit.snapshot())
}

async fn command(
    State(cockpit): State<Arc<Cockpit>>,
    Json(command): Json<ControlCommand>,
) -> Json<ControlAck> {
    let Some(path) = &cockpit.controls_path else {
        return Json(ControlAck::refused(
            "the cockpit is read-only: start it with --controls <log>",
        ));
    };
    let mission = command.mission_id().map(str::to_string);
    let summary = format!("operator command: {command:?}");
    let ack = match control::submit(path, command) {
        Ok(ack) => ack,
        Err(error) => ControlAck::refused(error.to_string()),
    };
    let seq = read_events(&cockpit.events_path)
        .ok()
        .and_then(|events| events.last().map(|event| event.seq))
        .unwrap_or(0)
        .saturating_add(1);
    let mut event = Event::new(seq, now_ms(), EventKind::ControlReceived, summary);
    if let Some(mission) = mission {
        event = event.with_mission(mission);
    }
    let _ = append_event(&cockpit.events_path, &event);
    Json(ack)
}

async fn events(
    State(cockpit): State<Arc<Cockpit>>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let stream =
        futures::stream::unfold((cockpit, interval), |(cockpit, mut interval)| async move {
            interval.tick().await;
            let data = serde_json::to_string(&cockpit.snapshot()).unwrap_or_default();
            Some((
                Ok::<_, std::convert::Infallible>(SseEvent::default().data(data)),
                (cockpit, interval),
            ))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlState;
    use crate::event::{append_event, EventKind};
    use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use crate::pricing::TokenUsage;
    use std::net::SocketAddr;

    fn entry(mission: &str, task: &str, micros: u64) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: mission.into(),
            task: task.into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        })
    }

    fn cockpit(dir: &std::path::Path) -> Cockpit {
        let ledger_path = dir.join("ledger.jsonl");
        let events_path = dir.join("events.jsonl");
        append_entry(&ledger_path, &entry("m-1", "t-1", 100)).unwrap();
        append_entry(&ledger_path, &entry("m-1", "t-2", 200)).unwrap();
        append_entry(&ledger_path, &entry("m-2", "t-3", 50)).unwrap();
        append_event(
            &events_path,
            &Event::new(1, 10, EventKind::MissionStarted, "started").with_mission("m-1"),
        )
        .unwrap();
        Cockpit {
            ledger_path,
            events_path,
            state_path: None,
            plan_path: None,
            controls_path: None,
            catalogue_path: None,
            graph_path: None,
            budget: None,
            mission: None,
        }
    }

    #[test]
    fn cockpit_graph_appears_beside_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let cockpit = cockpit(dir.path());
        assert!(cockpit.snapshot().graph.is_empty());
        std::fs::write(
            dir.path().join("graph.jsonl"),
            "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{}}\n\
             {\"from\":\"reviewer\",\"to\":\"coders\",\"kind\":\"approval\",\"payload\":{}}\n",
        )
        .unwrap();
        let snapshot = cockpit.snapshot();
        assert_eq!(snapshot.graph.messages.len(), 2);
        assert_eq!(snapshot.graph.roles, vec!["planner", "reviewer"]);
        assert!(snapshot.graph.approved());
    }

    #[test]
    fn a_named_graph_supplies_roles_and_per_role_spend() {
        let dir = tempfile::tempdir().unwrap();
        let mut cockpit = cockpit(dir.path());
        cockpit.mission = Some("m-1".into());
        append_entry(&cockpit.ledger_path, &entry("m-1", "planner", 700)).unwrap();
        append_entry(&cockpit.ledger_path, &entry("m-2", "planner", 900)).unwrap();
        let graph = dir.path().join("mission-graph.jsonl");
        std::fs::write(
            &graph,
            "{\"from\":\"planner\",\"to\":\"coders\",\"kind\":\"plan\",\"payload\":{}}\n\
             {\"from\":\"coders\",\"to\":\"reviewer\",\"kind\":\"work\",\"payload\":{}}\n\
             {\"from\":\"reviewer\",\"to\":\"planner\",\"kind\":\"approval\",\"payload\":{}}\n",
        )
        .unwrap();
        cockpit.graph_path = Some(graph);
        let snapshot = cockpit.snapshot();
        assert_eq!(snapshot.graph.roles, vec!["coders", "planner", "reviewer"]);
        assert_eq!(
            snapshot
                .role_spend
                .iter()
                .map(|spend| spend.role.as_str())
                .collect::<Vec<_>>(),
            vec!["coders", "planner", "reviewer"]
        );
        let planner = snapshot
            .role_spend
            .iter()
            .find(|spend| spend.role == "planner")
            .unwrap();
        assert_eq!(planner.calls, 1);
        assert_eq!(planner.cost_micro_usd, 700);
        let coders = snapshot
            .role_spend
            .iter()
            .find(|spend| spend.role == "coders")
            .unwrap();
        assert_eq!(coders.calls, 0);
        assert_eq!(coders.cost_micro_usd, 0);
    }

    #[test]
    fn a_snapshot_groups_and_sums_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = cockpit(dir.path()).snapshot();
        assert_eq!(snapshot.total_cost_micro_usd, 350);
        assert_eq!(snapshot.missions.len(), 2);
        let first = &snapshot.missions[0];
        assert_eq!(first.mission, "m-1");
        assert_eq!(first.calls, 2);
        assert_eq!(first.tasks, 2);
        assert_eq!(first.cost_micro_usd, 300);
        assert_eq!(first.state, "running");
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.workers.len(), 1);
        assert_eq!(snapshot.workers[0].calls, 3);
    }

    #[test]
    fn a_plan_and_state_show_progress_and_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut cockpit = cockpit(dir.path());
        let plan = dir.path().join("plan.json");
        std::fs::write(
            &plan,
            r#"{"mission":"m-1","tasks":[{"key":"a"},{"key":"b"},{"key":"c"}]}"#,
        )
        .unwrap();
        let state = dir.path().join("state.json");
        std::fs::write(
            &state,
            r#"{"records":{"a":{"status":"done"},"b":{"status":"failed","reason":"no"}}}"#,
        )
        .unwrap();
        cockpit.plan_path = Some(plan);
        cockpit.state_path = Some(state);
        cockpit.budget = Some(Budget::new(MicroUsd::from_micros(1_000)));
        let snapshot = cockpit.snapshot();
        let mission = snapshot
            .missions
            .iter()
            .find(|mission| mission.mission == "m-1")
            .unwrap();
        assert_eq!(mission.planned, 3);
        assert_eq!(mission.done, 1);
        assert_eq!(mission.failed, 1);
        assert_eq!(snapshot.budget_micro_usd, Some(1_000));
        assert_eq!(snapshot.budget_remaining_micro_usd, Some(700));
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("failed")));
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("no activity")));
    }

    #[test]
    fn a_plan_that_never_started_is_warned() {
        let dir = tempfile::tempdir().unwrap();
        let plan = dir.path().join("plan.json");
        std::fs::write(
            &plan,
            r#"{"mission":"m-9","tasks":[{"key":"a"},{"key":"b"}]}"#,
        )
        .unwrap();
        let cockpit = Cockpit {
            ledger_path: dir.path().join("ledger.jsonl"),
            events_path: dir.path().join("events.jsonl"),
            state_path: None,
            plan_path: Some(plan),
            controls_path: None,
            catalogue_path: None,
            graph_path: None,
            budget: None,
            mission: None,
        };
        let snapshot = cockpit.snapshot();
        let mission = &snapshot.missions[0];
        assert_eq!(mission.mission, "m-9");
        assert_eq!(mission.planned, 2);
        assert_eq!(mission.state, "reported");
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("never started")));
    }

    #[test]
    fn an_operator_pause_is_reflected_and_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let mut cockpit = cockpit(dir.path());
        let controls = dir.path().join("controls.jsonl");
        cockpit.controls_path = Some(controls.clone());
        control::submit(
            &controls,
            ControlCommand::Pause {
                mission_id: "m-1".into(),
            },
        )
        .unwrap();
        append_event(
            &cockpit.events_path,
            &Event::new(2, 20, EventKind::MissionPaused, "paused").with_mission("m-1"),
        )
        .unwrap();
        let snapshot = cockpit.snapshot();
        assert_eq!(snapshot.controls.len(), 1);
        assert!(ControlState::from_records(&snapshot.controls).paused);
        let mission = snapshot
            .missions
            .iter()
            .find(|mission| mission.mission == "m-1")
            .unwrap();
        assert_eq!(mission.state, "paused");
        assert!(snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("paused")));
    }

    #[tokio::test]
    async fn the_router_serves_status_and_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let cockpit = Arc::new(cockpit(dir.path()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, super::router(cockpit)).await;
        });

        let status = get_text(address, "/api/status").await;
        assert!(status.contains("\"m-1\""));
        assert!(status.contains("350"));
        assert!(status.contains("\"workers\""));

        let panel = get_text(address, "/").await;
        assert!(panel.contains("susi cockpit"));
    }

    #[tokio::test]
    async fn a_command_over_http_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut cockpit = cockpit(dir.path());
        let controls = dir.path().join("controls.jsonl");
        cockpit.controls_path = Some(controls.clone());
        let cockpit = Arc::new(cockpit);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, super::router(cockpit)).await;
        });
        let body = r#"{"command":"pause","mission_id":"m-1"}"#;
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let response = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut collected = Vec::new();
            let _ = stream.read_to_end(&mut collected);
            String::from_utf8_lossy(&collected).into_owned()
        })
        .await
        .unwrap();
        assert!(response.contains("\"accepted\":true"), "{response}");
        let records = control::read_records(&controls);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command.mission_id(), Some("m-1"));
    }

    #[tokio::test]
    async fn the_event_route_streams_sse() {
        let dir = tempfile::tempdir().unwrap();
        let cockpit = Arc::new(cockpit(dir.path()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, super::router(cockpit)).await;
        });

        let text = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            stream
                .write_all(b"GET /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut collected = Vec::new();
            loop {
                let mut buffer = [0u8; 1024];
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        collected.extend_from_slice(&buffer[..read]);
                        if String::from_utf8_lossy(&collected).contains("data:") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&collected).into_owned()
        })
        .await
        .unwrap();
        assert!(text.contains("text/event-stream"));
        assert!(text.contains("data:"));
    }

    async fn get_text(address: SocketAddr, path: &str) -> String {
        let url = format!("http://{address}{path}");
        tokio::task::spawn_blocking(move || {
            let call = susi_http_transport::http_call("GET", &url, &[], 5, 0).unwrap();
            assert_eq!(call.status, 200);
            call.into_utf8(64 * 1024).unwrap()
        })
        .await
        .unwrap()
    }
}
