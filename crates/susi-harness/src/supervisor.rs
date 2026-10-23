//! The supervisor loop.
//!
//! A plan lists the tasks of one mission. The supervisor runs each task
//! through a [`TaskExecutor`], persists the result after every task, and on a
//! rerun skips what is already done — so a crashed or interrupted mission
//! resumes instead of restarting.
//!
//! The susi executor ([`SusiExecutor`]) is the production implementation: it
//! prepares a worktree, claims the task, runs the agent, verifies, and lands
//! it. Tests use a fake executor, so the loop is exercised without a repo.

use crate::adapter::{
    land, run_task, CommandRunner, SusiRunPolicy, SusiRunReport, SusiTarget, SusiTaskSpec,
};
use crate::budget::{Budget, BudgetGuard};
use crate::catalog::PriceCatalogue;
use crate::control::ControlState;
use crate::executor::{RunOutcome, Runner};
use crate::lifecycle::{self, LifecycleEvent};
use crate::policy::Policy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

/// One task in a supervisor plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanTask {
    /// Stable key within the plan (also the worktree suffix and state key).
    pub key: String,
    /// Task title handed to `susi tasks add` (or the queue task's title).
    pub title: String,
    /// The task's acceptance command.
    pub accept: String,
    /// Paths the task reserves.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Worker model for this task. When absent, the executor's model is used.
    ///
    /// One mission can therefore run a DeepSeek worker on one task and a
    /// Gemini worker on the next.
    #[serde(default)]
    pub model: Option<String>,
    /// An existing queue task to claim instead of minting a new one.
    ///
    /// When set, the worktree already carries the task record, the adapter
    /// claims that id, and the acceptance comes from the record.
    #[serde(default)]
    pub task: Option<String>,
    /// The instruction for the agent.
    pub prompt: String,
}

/// A mission plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Plan {
    /// Mission id.
    pub mission: String,
    /// Tasks, in run order.
    pub tasks: Vec<PlanTask>,
}

impl Plan {
    /// Parse and validate a plan document.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` for malformed JSON, an empty plan, a blank
    /// field, a missing scope, or a duplicate key.
    pub fn from_json(text: &str) -> EaiResult<Self> {
        let plan: Self = serde_json::from_str(text)
            .map_err(|error| EaiError::config(format!("plan: {error}")))?;
        plan.validate()?;
        Ok(plan)
    }

    /// Read and validate a plan file.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the file cannot be read, or the same errors
    /// as [`Plan::from_json`].
    pub fn load(path: &Path) -> EaiResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| EaiError::io(format!("read plan {}: {error}", path.display())))?;
        Self::from_json(&text)
    }

    /// Validate the plan's shape: a mission, at least one task, unique keys,
    /// and no empty acceptance, prompt or scope.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` naming the first violation.
    pub fn validate(&self) -> EaiResult<()> {
        if self.mission.trim().is_empty() {
            return Err(EaiError::config("plan mission is required"));
        }
        if self.tasks.is_empty() {
            return Err(EaiError::config("plan has no tasks"));
        }
        let mut keys = BTreeSet::new();
        for task in &self.tasks {
            if task.key.trim().is_empty() {
                return Err(EaiError::config("plan task key is required"));
            }
            if task.title.trim().is_empty() {
                return Err(EaiError::config(format!("task {} has no title", task.key)));
            }
            if task.accept.trim().is_empty() {
                return Err(EaiError::config(format!(
                    "task {} has no acceptance",
                    task.key
                )));
            }
            if task.prompt.trim().is_empty() {
                return Err(EaiError::config(format!("task {} has no prompt", task.key)));
            }
            if task.scopes.is_empty() {
                return Err(EaiError::config(format!(
                    "task {} reserves no scopes",
                    task.key
                )));
            }
            if !keys.insert(task.key.clone()) {
                return Err(EaiError::config(format!(
                    "task key {} appears twice",
                    task.key
                )));
            }
        }
        Ok(())
    }
}

/// How one task ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// The task was prepared, accepted and landed.
    Done {
        /// The task id the target minted.
        task: String,
    },
    /// The task did not land.
    Failed {
        /// Why.
        reason: String,
    },
}

/// What the supervisor did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorReport {
    /// Mission id.
    pub mission: String,
    /// Task keys that landed this run.
    pub done: Vec<String>,
    /// Task keys that failed this run, with the reason.
    pub failed: Vec<(String, String)>,
    /// Task keys skipped because an earlier run landed them.
    pub skipped: Vec<String>,
    /// Task keys not started because the mission budget was exhausted.
    pub unstarted: Vec<String>,
    /// True when the budget stopped the run early.
    pub budget_exhausted: bool,
    /// True when the operator paused the mission.
    pub paused: bool,
    /// True when the operator aborted the mission.
    pub aborted: bool,
}

/// Prepares and lands a planned task.
///
/// Preparation is the expensive, parallel half (worktree, claim, agent,
/// verification); landing is the serial half (close, push, merge) and runs on
/// the supervisor's thread.
pub trait TaskExecutor: Sync {
    /// What a prepared task carries to the landing step.
    type Prepared: Send;

    /// Prepare one task: everything up to and including verification.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error; a task that simply did not
    /// pass is still `Ok`.
    fn prepare(&self, task: &PlanTask) -> EaiResult<Self::Prepared>;

    /// Land a prepared task and report its final status.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error; a task that simply did not
    /// land is [`TaskStatus::Failed`].
    fn land(&self, prepared: Self::Prepared) -> EaiResult<TaskStatus>;

    /// True once the mission budget refused an attempt.
    ///
    /// The supervisor checks this after every task and stops the plan instead
    /// of starting work the mission cannot pay for.
    fn budget_exhausted(&self) -> bool {
        false
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    records: BTreeMap<String, Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    status: String,
    task: Option<String>,
    reason: Option<String>,
}

#[derive(Default)]
struct Tally {
    done: Vec<String>,
    failed: Vec<(String, String)>,
}

/// Runs a plan, persisting after every task.
pub struct Supervisor<'a, E: TaskExecutor> {
    /// Where the resume state lives.
    pub state_path: PathBuf,
    /// How tasks are executed.
    pub executor: &'a E,
    /// How many tasks prepare at once (landing is always serial).
    pub max_parallel: usize,
    /// Operator control log, when the cockpit can pause or abort the mission.
    pub control_path: Option<PathBuf>,
}

impl<'a, E: TaskExecutor> Supervisor<'a, E> {
    /// Build a supervisor that prepares one task at a time.
    #[must_use]
    pub fn new(state_path: impl Into<PathBuf>, executor: &'a E) -> Self {
        Self {
            state_path: state_path.into(),
            executor,
            max_parallel: 1,
            control_path: None,
        }
    }

    /// Set how many tasks prepare at once.
    #[must_use]
    pub fn with_parallelism(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel.max(1);
        self
    }

    /// Read operator commands from this log before each batch.
    #[must_use]
    pub fn with_controls(mut self, path: impl Into<PathBuf>) -> Self {
        self.control_path = Some(path.into());
        self
    }

    /// Run the plan, skipping tasks an earlier run landed.
    ///
    /// Tasks are prepared in batches of `max_parallel`; each batch's results
    /// are then landed serially, in plan order, and persisted one by one.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the state cannot be read or written.
    pub fn run(&self, plan: &Plan) -> EaiResult<SupervisorReport> {
        let mut state = load_state(&self.state_path)?;
        let mut tally = Tally::default();
        let mut skipped = Vec::new();
        let pending: Vec<&PlanTask> = plan
            .tasks
            .iter()
            .filter(|task| {
                let finished = state
                    .records
                    .get(&task.key)
                    .is_some_and(|record| record.status == "done");
                if finished {
                    skipped.push(task.key.clone());
                }
                !finished
            })
            .collect();
        let mut unstarted = Vec::new();
        let mut budget_exhausted = false;
        let mut paused = false;
        let mut aborted = false;
        let parallel = self.max_parallel.max(1);
        'plan: for (batch_index, batch) in pending.chunks(parallel).enumerate() {
            if let Some(path) = &self.control_path {
                let controls = ControlState::load(path);
                if controls.blocks_new_work() {
                    paused = controls.paused;
                    aborted = controls.aborted;
                    let consumed = batch_index * parallel;
                    unstarted = pending
                        .iter()
                        .skip(consumed)
                        .map(|task| task.key.clone())
                        .collect();
                    break 'plan;
                }
            }
            for (offset, (task, result)) in self.prepare_batch(batch).into_iter().enumerate() {
                let status = match result {
                    Ok(prepared) => self.executor.land(prepared),
                    Err(error) => Ok(TaskStatus::Failed {
                        reason: error.to_string(),
                    }),
                }
                .unwrap_or_else(|error| TaskStatus::Failed {
                    reason: error.to_string(),
                });
                let stop = self.executor.budget_exhausted();
                record(&mut state, &self.state_path, task, status, &mut tally)?;
                if stop {
                    budget_exhausted = true;
                    let consumed = batch_index * parallel + offset + 1;
                    unstarted = pending
                        .iter()
                        .skip(consumed)
                        .map(|task| task.key.clone())
                        .collect();
                    break 'plan;
                }
            }
        }
        Ok(SupervisorReport {
            mission: plan.mission.clone(),
            done: tally.done,
            failed: tally.failed,
            skipped,
            unstarted,
            budget_exhausted,
            paused,
            aborted,
        })
    }

    fn prepare_batch<'b>(
        &self,
        batch: &[&'b PlanTask],
    ) -> Vec<(&'b PlanTask, EaiResult<E::Prepared>)> {
        if batch.len() == 1 {
            let task = batch[0];
            return vec![(task, self.executor.prepare(task))];
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|task| (*task, scope.spawn(move || self.executor.prepare(task))))
                .collect();
            handles
                .into_iter()
                .map(|(task, handle)| {
                    let result = handle
                        .join()
                        .unwrap_or_else(|_| Err(EaiError::process("prepare thread panicked")));
                    (task, result)
                })
                .collect()
        })
    }
}

fn record(
    state: &mut State,
    state_path: &Path,
    task: &PlanTask,
    status: TaskStatus,
    tally: &mut Tally,
) -> EaiResult<()> {
    let stored = match &status {
        TaskStatus::Done { task: id } => Record {
            status: "done".into(),
            task: Some(id.clone()),
            reason: None,
        },
        TaskStatus::Failed { reason } => Record {
            status: "failed".into(),
            task: None,
            reason: Some(reason.clone()),
        },
    };
    state.records.insert(task.key.clone(), stored);
    save_state(state_path, state)?;
    match status {
        TaskStatus::Done { .. } => tally.done.push(task.key.clone()),
        TaskStatus::Failed { reason } => tally.failed.push((task.key.clone(), reason)),
    }
    Ok(())
}

/// The production executor: one susi task through the adapter.
pub struct SusiExecutor<'a, R: CommandRunner> {
    /// The prepared target.
    pub target: &'a SusiTarget<R>,
    /// The agent runner.
    pub runner: &'a dyn Runner,
    /// Worktree name prefix.
    pub worktree_prefix: String,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Model id used when a task does not name one.
    pub model: String,
    /// Models to escalate to when the preferred one does not pass.
    pub alternates: Vec<String>,
    /// Price catalogue used to rank alternates and estimate spend.
    pub catalogue: Option<PriceCatalogue>,
    /// Mission budget; when absent the mission is unmetered.
    pub budget: Option<Budget>,
    /// Ledger the gateway writes (spend authority).
    pub ledger_path: PathBuf,
    /// Mission id every attempt is billed to.
    pub mission: String,
    /// Escalation policy.
    pub policy: Policy,
    /// Per-turn deadline.
    pub timeout: Duration,
    /// Per-task adapter state root.
    pub state_dir: PathBuf,
    /// Self-build lifecycle ledger the scorecard reads; `None` disables it.
    pub lifecycle_path: Option<PathBuf>,
    /// Built on the first prepare when a budget is set.
    pub guard: std::sync::OnceLock<BudgetGuard>,
    /// Set once the budget refused an attempt.
    pub exhausted: AtomicBool,
}

impl<R: CommandRunner> TaskExecutor for SusiExecutor<'_, R> {
    type Prepared = SusiRunReport;

    fn prepare(&self, task: &PlanTask) -> EaiResult<SusiRunReport> {
        let preferred = task.model.clone().unwrap_or_else(|| self.model.clone());
        // The mission's default model is a worker on every task, even when the
        // task names a different one: otherwise a task assigned the cheap
        // worker has no stronger rung to escalate to.
        let mut alternates = self.alternates.clone();
        if !alternates.iter().any(|candidate| candidate == &self.model) {
            alternates.push(self.model.clone());
        }
        let spec = SusiTaskSpec {
            worktree_name: format!("{}-{}", self.worktree_prefix, task.key),
            title: task.title.clone(),
            accept: task.accept.clone(),
            scopes: task.scopes.clone(),
            prompt: task.prompt.clone(),
            gateway_url: self.gateway_url.clone(),
            model: preferred,
            alternates,
            timeout: self.timeout,
            state_dir: self.state_dir.join(&task.key),
            task: task.task.clone(),
        };
        let guard = self
            .budget
            .map(|budget| self.guard.get_or_init(|| BudgetGuard::new(budget)));
        let run_policy = SusiRunPolicy {
            catalogue: self.catalogue.as_ref(),
            guard,
            ledger_path: &self.ledger_path,
            mission: &self.mission,
            policy: self.policy,
        };
        let report = run_task(self.target, &spec, self.runner, &run_policy)?;
        if report.budget_exhausted {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        self.emit_work(&report);
        Ok(report)
    }

    fn land(&self, report: SusiRunReport) -> EaiResult<TaskStatus> {
        if !report.accepted {
            return Ok(TaskStatus::Failed {
                reason: format!(
                    "{} attempt(s); {} / committed {} / acceptance not passed",
                    report.attempts.len(),
                    describe(&report.agent),
                    report.committed
                ),
            });
        }
        land(self.target, &report)?;
        self.emit_merged(&report);
        Ok(TaskStatus::Done { task: report.task })
    }

    fn budget_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::SeqCst)
    }
}

impl<R: CommandRunner> SusiExecutor<'_, R> {
    /// Record what the prepare phase proved, before landing.
    fn emit_work(&self, report: &SusiRunReport) {
        let Some(path) = &self.lifecycle_path else {
            return;
        };
        let detail = format!(
            "{} attempt(s); {}",
            report.attempts.len(),
            describe(&report.agent)
        );
        for event in work_events(
            &report.task,
            report.committed,
            report.accepted,
            detail,
            lifecycle::now_unix(),
        ) {
            write_lifecycle(path, &event);
        }
    }

    /// Record the merge and what the task cost, once landing returned.
    ///
    /// `workflow finish` returns only after the accepted close is published, so
    /// this point is the first one where the harness can truthfully say
    /// `merged` rather than `pushed`.
    fn emit_merged(&self, report: &SusiRunReport) {
        let Some(path) = &self.lifecycle_path else {
            return;
        };
        let cost = lifecycle::task_cost(&self.ledger_path, &self.mission, &report.task);
        write_lifecycle(
            path,
            &LifecycleEvent::merged(&report.task, lifecycle::now_unix(), cost),
        );
    }
}

/// A lifecycle write must never fail a mission that otherwise succeeded: the
/// scorecard is evidence, not a gate. A broken path is reported on stderr.
fn write_lifecycle(path: &Path, event: &LifecycleEvent) {
    if let Err(error) = lifecycle::append(path, event) {
        eprintln!("harness: lifecycle write failed: {error}");
    }
}

/// The events a prepared task earns: a committed change is `implemented`, a
/// passing acceptance is `gated`, and anything else is a `defect` carrying the
/// reason. Pure, so the decision is testable without a repository.
fn work_events(
    task: &str,
    committed: bool,
    accepted: bool,
    detail: String,
    at_unix: u64,
) -> Vec<LifecycleEvent> {
    let mut events = Vec::new();
    if committed {
        events.push(LifecycleEvent::new(task, "implemented", at_unix));
    }
    if accepted {
        events.push(LifecycleEvent::new(task, "gated", at_unix));
    } else {
        events.push(LifecycleEvent::defect(
            task,
            at_unix,
            "not-accepted",
            detail,
        ));
    }
    events
}

fn describe(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Succeeded { .. } => "succeeded".to_string(),
        RunOutcome::Failed { message } => format!("failed: {message}"),
        RunOutcome::TimedOut => "timed_out".to_string(),
    }
}

fn load_state(path: &Path) -> EaiResult<State> {
    if !path.is_file() {
        return Ok(State::default());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| EaiError::io(format!("read state {}: {error}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|error| EaiError::io(format!("parse state {}: {error}", path.display())))
}

fn save_state(path: &Path, state: &State) -> EaiResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| EaiError::io(format!("create {}: {error}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| EaiError::io(format!("encode state: {error}")))?;
    std::fs::write(path, text)
        .map_err(|error| EaiError::io(format!("write state {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct FakeExecutor {
        calls: Mutex<Vec<String>>,
        failing: Vec<String>,
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    impl FakeExecutor {
        fn new(failing: Vec<String>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                failing,
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }
        }
    }

    impl TaskExecutor for FakeExecutor {
        type Prepared = String;

        fn prepare(&self, task: &PlanTask) -> EaiResult<String> {
            self.calls.lock().unwrap().push(task.key.clone());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(80));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(task.key.clone())
        }

        fn land(&self, key: String) -> EaiResult<TaskStatus> {
            if self.failing.contains(&key) {
                Ok(TaskStatus::Failed {
                    reason: "no".into(),
                })
            } else {
                Ok(TaskStatus::Done {
                    task: format!("T-{key}"),
                })
            }
        }
    }

    fn plan() -> Plan {
        Plan {
            mission: "m-1".into(),
            tasks: ["a", "b", "c"]
                .iter()
                .map(|key| PlanTask {
                    key: (*key).into(),
                    title: format!("task {key}"),
                    accept: "cargo test -p susi-harness".into(),
                    scopes: vec!["docs/harness.md".into()],
                    model: None,
                    task: None,
                    prompt: "do it".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_run_lands_every_task_and_persists_state() {
        let dir = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::new(vec![]);
        let supervisor = Supervisor::new(dir.path().join("state.json"), &executor);
        let report = supervisor.run(&plan()).unwrap();
        assert_eq!(report.done, vec!["a", "b", "c"]);
        assert!(report.failed.is_empty());
        assert!(report.skipped.is_empty());
        assert!(dir.path().join("state.json").is_file());
    }

    #[test]
    fn a_rerun_skips_what_landed() {
        let dir = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::new(vec![]);
        let supervisor = Supervisor::new(dir.path().join("state.json"), &executor);
        supervisor.run(&plan()).unwrap();
        let second = supervisor.run(&plan()).unwrap();
        assert_eq!(second.skipped, vec!["a", "b", "c"]);
        assert_eq!(executor.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn a_failed_task_is_retried_on_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let failing = FakeExecutor::new(vec!["b".into()]);
        let state = dir.path().join("state.json");
        let first = Supervisor::new(&state, &failing).run(&plan()).unwrap();
        assert_eq!(first.done, vec!["a", "c"]);
        assert_eq!(first.failed.len(), 1);

        let retry = FakeExecutor::new(vec![]);
        let second = Supervisor::new(&state, &retry).run(&plan()).unwrap();
        assert_eq!(second.done, vec!["b"]);
        assert_eq!(second.skipped, vec!["a", "c"]);
    }

    #[test]
    fn tasks_prepare_in_parallel_and_land_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::new(vec![]);
        let supervisor =
            Supervisor::new(dir.path().join("state.json"), &executor).with_parallelism(2);
        let report = supervisor.run(&plan()).unwrap();
        assert_eq!(report.done, vec!["a", "b", "c"]);
        let peak = executor.peak.load(Ordering::SeqCst);
        assert!(
            peak >= 2,
            "expected two prepares in flight, peak was {peak}"
        );
    }

    #[test]
    fn a_plan_is_validated() {
        let duplicate = r#"{
            "mission": "m",
            "tasks": [
                {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p"},
                {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p"}
            ]
        }"#;
        assert!(Plan::from_json(duplicate).is_err());

        let no_scopes = r#"{
            "mission": "m",
            "tasks": [{"key":"a","title":"t","accept":"cargo test","prompt":"p"}]
        }"#;
        assert!(Plan::from_json(no_scopes).is_err());
        assert!(Plan::from_json("{}").is_err());
    }

    fn phases(events: &[LifecycleEvent]) -> Vec<&str> {
        events.iter().map(|event| event.phase.as_str()).collect()
    }

    #[test]
    fn a_prepared_task_earns_its_lifecycle_events() {
        let accepted = work_events("T-1", true, true, "ok".into(), 10);
        assert_eq!(phases(&accepted), vec!["implemented", "gated"]);

        let rejected = work_events("T-1", true, false, "tests red".into(), 10);
        assert_eq!(phases(&rejected), vec!["implemented", "defect"]);
        assert_eq!(rejected[1].outcome.as_deref(), Some("not-accepted"));
        assert_eq!(rejected[1].detail.as_deref(), Some("tests red"));

        let nothing = work_events("T-1", false, false, "no work".into(), 10);
        assert_eq!(phases(&nothing), vec!["defect"]);
    }
}
