//! The mission scheduler.
//!
//! A mission is a set of tasks; each task is one headless turn tried against
//! the mission's candidate models. The candidates are **ranked by expected cost
//! per accepted task** ([`crate::policy`]) and tried cheapest-first. Up to
//! `max_parallel` tasks run at once.
//!
//! Two guards keep spend honest:
//!
//! - Before a turn is dispatched, [`BudgetGuard`] must admit its worst-case
//!   estimate against what the ledger already shows and what the other
//!   in-flight turns reserved. A refused turn is reported as
//!   [`AttemptOutcome::Skipped`]; the task is left for a top-up, never retried
//!   into the wall.
//! - The ladder escalates on failure, retries a transient provider error at
//!   most once on the same model, and stops when two models fail the same way.
//!
//! The gateway prices every call (so the ledger is the spend record) and this
//! module writes the mission's narrative to `events.jsonl` for the cockpit.
//!
//! The scheduler depends on [`Runner`], not on a concrete worker, so a mission
//! can be tested without a gateway.

use crate::budget::{Admission, Budget, BudgetGuard};
use crate::catalog::PriceCatalogue;
use crate::event::{append_event, Event, EventKind};
use crate::executor::{ExecutorConfig, RunOutcome, Runner};
use crate::ledger::{read_entries_lenient, read_entries_reporting};
use crate::money::MicroUsd;
use crate::policy::{
    history, rank_models, FailureKind, Ladder, NextAttempt, Policy, StopReason, UsageProfile,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// One task in a mission.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// Task id, unique within the mission.
    pub id: String,
    /// The instruction handed to the agent.
    pub prompt: String,
    /// Working directory the agent reads and writes.
    pub workspace: PathBuf,
}

/// How a task's ladder stopped without succeeding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStop {
    /// The next attempt would have crossed the mission budget.
    Budget,
    /// The ladder's own limit: attempts used up, or two models failed alike.
    Ladder(StopReason),
}

/// Everything a mission needs to run.
#[derive(Debug, Clone)]
pub struct MissionConfig {
    /// Mission id; also the ledger tag every call carries.
    pub id: String,
    /// Tasks to run.
    pub tasks: Vec<TaskSpec>,
    /// Candidate models; ranked by expected cost when a catalogue is present.
    pub candidates: Vec<String>,
    /// Maximum tasks in flight at once.
    pub max_parallel: usize,
    /// Per-attempt deadline.
    pub timeout: Duration,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Per-mission state root; each task gets `<state>/<task>/attempt-<n>`.
    pub state_dir: PathBuf,
    /// Ledger the gateway writes, read back for spend and history.
    pub ledger_path: PathBuf,
    /// Optional per-task spending limit.
    pub budget: Option<Budget>,
    /// Escalation policy.
    pub policy: Policy,
    /// Token profile used to compare models before a task has run.
    pub usage: UsageProfile,
    /// Price catalogue used to rank and estimate; required with a budget.
    pub catalogue: Option<PriceCatalogue>,
}

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// The turn produced a final answer.
    Succeeded {
        /// The agent's final text.
        final_text: String,
    },
    /// The agent reported failure.
    Failed {
        /// Why.
        message: String,
    },
    /// The deadline expired.
    TimedOut,
    /// The executor itself failed (spawn, I/O).
    Error {
        /// Why.
        message: String,
    },
    /// The attempt was not run because it would have crossed the budget.
    Skipped {
        /// Why, in the operator's language.
        reason: String,
    },
}

/// One model attempt on one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptResult {
    /// Model tried.
    pub model: String,
    /// How it ended.
    pub outcome: AttemptOutcome,
    /// Wall-clock duration.
    pub elapsed_ms: u64,
    /// Pessimistic cost of the attempt, when the catalogue could price it.
    pub estimate: Option<MicroUsd>,
}

/// One task's final state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResult {
    /// Task id.
    pub task: String,
    /// Every attempt, in order.
    pub attempts: Vec<AttemptResult>,
    /// Whether any attempt succeeded.
    pub succeeded: bool,
    /// Set when the ladder stopped without a success.
    pub stopped: Option<TaskStop>,
}

impl TaskResult {
    /// The successful attempt's final text, when there is one.
    #[must_use]
    pub fn final_text(&self) -> Option<&str> {
        self.attempts
            .iter()
            .rev()
            .find_map(|attempt| match &attempt.outcome {
                AttemptOutcome::Succeeded { final_text } => Some(final_text.as_str()),
                AttemptOutcome::Failed { .. }
                | AttemptOutcome::TimedOut
                | AttemptOutcome::Error { .. }
                | AttemptOutcome::Skipped { .. } => None,
            })
    }

    /// True when the budget, not the ladder, is what stopped this task.
    #[must_use]
    pub fn budget_stopped(&self) -> bool {
        self.stopped == Some(TaskStop::Budget)
    }
}

/// What the mission produced.
#[derive(Debug, Clone)]
pub struct MissionReport {
    /// Mission id.
    pub mission: String,
    /// Per-task results, sorted by task id.
    pub results: Vec<TaskResult>,
    /// Tasks that succeeded.
    pub succeeded: usize,
    /// Tasks that did not.
    pub failed: usize,
    /// Total priced spend recorded for this mission.
    pub total_cost: MicroUsd,
    /// The mission's limit, when it had one.
    pub budget_limit: Option<MicroUsd>,
    /// True when at least one task was left undone for lack of budget.
    pub budget_exhausted: bool,
    /// Tasks the ladder gave up on (repeated failure or attempt cap).
    pub stuck: usize,
    /// Event feed written for the cockpit.
    pub events_path: PathBuf,
}

/// Run a mission to completion.
///
/// # Errors
///
/// Returns `EaiError::config` for a mission that cannot start (no candidates,
/// no price catalogue with a budget, or an unpriceable candidate), and
/// `EaiError::io` when the state root cannot be created.
pub fn run_mission(config: &MissionConfig, runner: &dyn Runner) -> EaiResult<MissionReport> {
    if config.budget.is_some() && config.catalogue.is_none() {
        return Err(EaiError::config(
            "a mission budget needs a price catalogue: an unpriced attempt cannot be bounded",
        ));
    }
    let ranked = match &config.catalogue {
        Some(catalogue) => {
            // One torn line must not erase the history the ranking learns
            // from: the strict reader failed the whole file on it.
            let entries = read_entries_reporting(&config.ledger_path);
            let history = history(&entries);
            rank_models(catalogue, &config.candidates, config.usage, &history)?
                .into_iter()
                .map(|candidate| candidate.model)
                .collect()
        }
        None => config.candidates.clone(),
    };
    if ranked.is_empty() {
        return Err(EaiError::config(
            "a mission needs at least one candidate model",
        ));
    }
    std::fs::create_dir_all(&config.state_dir).map_err(|error| {
        EaiError::io(format!(
            "create mission state {}: {error}",
            config.state_dir.display()
        ))
    })?;
    let events_path = config.state_dir.join("events.jsonl");
    let events = EventSink {
        path: events_path.clone(),
        seq: Mutex::new(0),
    };
    events.emit(
        EventKind::MissionStarted,
        &config.id,
        None,
        format!(
            "{} task(s), {} parallel, ladder {}; {}",
            config.tasks.len(),
            config.max_parallel,
            ranked.join(" -> "),
            match config.budget {
                Some(budget) => format!("budget {}", budget.limit),
                None => "no budget cap".to_string(),
            }
        ),
    );

    let guard = config.budget.map(BudgetGuard::new);
    let warned = AtomicBool::new(false);
    let queue = Mutex::new(config.tasks.iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    let workers = config.max_parallel.clamp(1, config.tasks.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let task = queue
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front();
                let Some(task) = task else {
                    break;
                };
                let result = run_task(
                    task,
                    config,
                    &ranked,
                    guard.as_ref(),
                    &warned,
                    runner,
                    &events,
                );
                results
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(result);
            });
        }
    });

    let mut results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    results.sort_by(|left, right| left.task.cmp(&right.task));
    let succeeded = results.iter().filter(|result| result.succeeded).count();
    let failed = results.len() - succeeded;
    let budget_exhausted = results.iter().any(TaskResult::budget_stopped);
    let stuck = results
        .iter()
        .filter(|result| matches!(result.stopped, Some(TaskStop::Ladder(_))))
        .count();
    events.emit(
        EventKind::MissionFinished,
        &config.id,
        None,
        format!("{succeeded} succeeded, {failed} failed"),
    );
    let total_cost = mission_cost(&config.ledger_path, &config.id);
    Ok(MissionReport {
        mission: config.id.clone(),
        results,
        succeeded,
        failed,
        total_cost,
        budget_limit: config.budget.map(|budget| budget.limit),
        budget_exhausted,
        stuck,
        events_path,
    })
}

#[allow(clippy::too_many_arguments)] // the mission's shared context plus the one task it runs; grouping them would only rename the same list
fn run_task(
    task: &TaskSpec,
    config: &MissionConfig,
    ranked: &[String],
    guard: Option<&BudgetGuard>,
    warned: &AtomicBool,
    runner: &dyn Runner,
    events: &EventSink,
) -> TaskResult {
    let mut attempts = Vec::new();
    let mut succeeded = false;
    let mut stopped = None;
    let mut ladder = Ladder::new(ranked.to_vec(), config.policy);
    loop {
        let attempt = match ladder.next_attempt() {
            NextAttempt::Attempt(attempt) => attempt,
            NextAttempt::Stop(reason) => {
                stopped = Some(TaskStop::Ladder(reason));
                break;
            }
        };
        let price = config
            .catalogue
            .as_ref()
            .and_then(|catalogue| catalogue.price(&attempt.model));
        let estimate = match (config.budget, price) {
            (Some(budget), Some(price)) => Some(budget.estimate(price, &task.prompt)),
            _ => None,
        };
        if let (Some(guard), Some(estimate)) = (guard, estimate) {
            let spent = Budget::spent(&config.ledger_path, &config.id);
            if let Admission::Exhausted {
                spent,
                estimate: wanted,
                limit,
            } = guard.admit(spent, estimate)
            {
                events.emit(
                    EventKind::BudgetExhausted,
                    &config.id,
                    Some(&task.id),
                    format!(
                        "{} not dispatched: {wanted} would cross {limit} (spent {spent})",
                        task.id
                    ),
                );
                attempts.push(AttemptResult {
                    model: attempt.model,
                    outcome: AttemptOutcome::Skipped {
                        reason: format!("budget: spent {spent} of {limit}"),
                    },
                    elapsed_ms: 0,
                    estimate: Some(estimate),
                });
                stopped = Some(TaskStop::Budget);
                break;
            }
        }
        events.emit(
            EventKind::TaskDispatched,
            &config.id,
            Some(&task.id),
            format!(
                "{} -> {} (attempt {}){}",
                task.id,
                attempt.model,
                attempt.number,
                estimate
                    .map(|estimate| format!(", reserved {estimate}"))
                    .unwrap_or_default()
            ),
        );
        let started = Instant::now();
        let outcome = runner.run(&ExecutorConfig {
            gateway_url: config.gateway_url.clone(),
            model: attempt.model.clone(),
            workspace: task.workspace.clone(),
            state_dir: config
                .state_dir
                .join(&task.id)
                .join(format!("attempt-{}", attempt.number)),
            mission: config.id.clone(),
            task: task.id.clone(),
            attempt: attempt.number,
            timeout: config.timeout,
            prompt: task.prompt.clone(),
        });
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let (Some(guard), Some(estimate)) = (guard, estimate) {
            let spent = Budget::spent(&config.ledger_path, &config.id);
            guard.settle(spent, estimate);
            if guard.budget().at_risk(spent) && !warned.swap(true, Ordering::SeqCst) {
                events.emit(
                    EventKind::BudgetWarning,
                    &config.id,
                    None,
                    format!(
                        "spent {} of {}; attempts stop when the next one would cross the cap",
                        spent.total,
                        guard.budget().limit
                    ),
                );
            }
        }
        let attempt_result = match outcome {
            Ok(RunOutcome::Succeeded { final_text, .. }) => {
                succeeded = true;
                AttemptResult {
                    model: attempt.model.clone(),
                    outcome: AttemptOutcome::Succeeded { final_text },
                    elapsed_ms,
                    estimate,
                }
            }
            Ok(RunOutcome::Failed { message }) => {
                let kind = FailureKind::classify(&message);
                let result = AttemptResult {
                    model: attempt.model.clone(),
                    outcome: AttemptOutcome::Failed {
                        message: message.clone(),
                    },
                    elapsed_ms,
                    estimate,
                };
                ladder.record(&attempt.model, kind, &message);
                result
            }
            Ok(RunOutcome::TimedOut) => {
                let result = AttemptResult {
                    model: attempt.model.clone(),
                    outcome: AttemptOutcome::TimedOut,
                    elapsed_ms,
                    estimate,
                };
                ladder.record(&attempt.model, FailureKind::Timeout, "timed out");
                result
            }
            Err(error) => {
                let message = error.to_string();
                let result = AttemptResult {
                    model: attempt.model.clone(),
                    outcome: AttemptOutcome::Error {
                        message: message.clone(),
                    },
                    elapsed_ms,
                    estimate,
                };
                ladder.record(&attempt.model, FailureKind::Executor, &message);
                result
            }
        };
        events.emit(
            EventKind::TaskFinished,
            &config.id,
            Some(&task.id),
            format!(
                "{} attempt {}: {}",
                task.id,
                attempt.number,
                describe(&attempt_result.outcome)
            ),
        );
        attempts.push(attempt_result);
        if succeeded {
            break;
        }
        if let Some(next) = ladder.current_model() {
            if next != attempt.model {
                events.emit(
                    EventKind::ModelEscalated,
                    &config.id,
                    Some(&task.id),
                    format!(
                        "{}: {} did not pass, escalating to {}",
                        task.id, attempt.model, next
                    ),
                );
            }
        }
    }
    TaskResult {
        task: task.id.clone(),
        attempts,
        succeeded,
        stopped,
    }
}

fn describe(outcome: &AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Succeeded { .. } => "succeeded",
        AttemptOutcome::Failed { .. } => "failed",
        AttemptOutcome::TimedOut => "timed out",
        AttemptOutcome::Error { .. } => "executor error",
        AttemptOutcome::Skipped { .. } => "skipped (budget)",
    }
}

/// The mission's spend, from the ledger's rows that can be read
/// ([`read_entries_lenient`]). The strict reader failed the whole file on one
/// torn line, a cut inside a multi-byte character or a row glued onto an
/// ASCII tear, and the mission then reported $0 spent.
fn mission_cost(path: &Path, mission: &str) -> MicroUsd {
    let (entries, _) = read_entries_lenient(path);
    entries
        .iter()
        .filter(|entry| entry.mission == mission)
        .fold(MicroUsd::ZERO, |sum, entry| {
            sum.checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX))
        })
}

struct EventSink {
    path: PathBuf,
    seq: Mutex<u64>,
}

impl EventSink {
    fn emit(&self, kind: EventKind, mission: &str, task: Option<&str>, summary: String) {
        let mut seq = self
            .seq
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *seq += 1;
        let mut event = Event::new(*seq, now_ms(), kind, summary).with_mission(mission);
        if let Some(task) = task {
            event = event.with_task(task);
        }
        if let Err(error) = append_event(&self.path, &event) {
            eprintln!("harness-mission: event log write failed: {error}");
        }
    }
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
    use crate::event::read_events;
    use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use crate::pricing::{ModelPrice, TokenUsage};

    struct FakeRunner {
        failing: Vec<String>,
        identical_failure: bool,
        calls: Mutex<Vec<String>>,
    }

    impl Runner for FakeRunner {
        fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            self.calls.lock().unwrap().push(config.model.clone());
            if self.failing.contains(&config.model) {
                let message = if self.identical_failure {
                    "acceptance failed".to_string()
                } else {
                    format!("{} failed", config.model)
                };
                return Ok(RunOutcome::Failed { message });
            }
            Ok(RunOutcome::Succeeded {
                final_text: format!("{} ok", config.model),
                reported_cost_usd: None,
            })
        }
    }

    fn runner(failing: Vec<&str>) -> FakeRunner {
        FakeRunner {
            failing: failing.into_iter().map(str::to_string).collect(),
            identical_failure: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn runner_with_identical_failures(failing: Vec<&str>) -> FakeRunner {
        FakeRunner {
            failing: failing.into_iter().map(str::to_string).collect(),
            identical_failure: true,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn catalogue() -> PriceCatalogue {
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "cheap".to_string(),
            ModelPrice {
                input_miss: MicroUsd::from_micros(100_000),
                input_hit: MicroUsd::from_micros(10_000),
                output: MicroUsd::from_micros(200_000),
            },
        );
        models.insert(
            "capable".to_string(),
            ModelPrice {
                input_miss: MicroUsd::from_micros(1_000_000),
                input_hit: MicroUsd::from_micros(100_000),
                output: MicroUsd::from_micros(2_000_000),
            },
        );
        PriceCatalogue {
            schema: crate::catalog::PRICE_SCHEMA.to_string(),
            models,
        }
    }

    fn mission(dir: &Path, max_parallel: usize, candidates: Vec<&str>) -> MissionConfig {
        MissionConfig {
            id: "m-1".into(),
            tasks: vec![
                TaskSpec {
                    id: "t-1".into(),
                    prompt: "one".into(),
                    workspace: dir.join("w1"),
                },
                TaskSpec {
                    id: "t-2".into(),
                    prompt: "two".into(),
                    workspace: dir.join("w2"),
                },
                TaskSpec {
                    id: "t-3".into(),
                    prompt: "three".into(),
                    workspace: dir.join("w3"),
                },
            ],
            candidates: candidates.into_iter().map(str::to_string).collect(),
            max_parallel,
            timeout: Duration::from_secs(5),
            gateway_url: "http://127.0.0.1:1".into(),
            state_dir: dir.to_path_buf(),
            ledger_path: dir.join("ledger.jsonl"),
            budget: None,
            policy: Policy::default(),
            usage: UsageProfile::agent(),
            catalogue: None,
        }
    }

    #[test]
    fn a_missing_state_root_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.state_dir = dir.path().join("nested").join("state");
        let report = run_mission(&config, &runner(vec![])).unwrap();
        assert!(report.events_path.exists());
        let events = read_events(&report.events_path).unwrap();
        assert_eq!(events.len(), 8);
        assert_eq!(
            events.first().map(|event| event.kind),
            Some(EventKind::MissionStarted)
        );
        assert_eq!(
            events.last().map(|event| event.kind),
            Some(EventKind::MissionFinished)
        );
    }

    #[test]
    fn every_task_runs_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let config = mission(dir.path(), 2, vec!["cheap"]);
        let report = run_mission(&config, &runner(vec![])).unwrap();
        assert_eq!(report.succeeded, 3);
        assert_eq!(report.failed, 0);
        assert!(report
            .results
            .iter()
            .all(|result| result.final_text() == Some("cheap ok")));
    }

    #[test]
    fn a_failing_model_falls_to_the_next_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let config = mission(dir.path(), 1, vec!["cheap", "capable"]);
        let report = run_mission(&config, &runner(vec!["cheap"])).unwrap();
        assert_eq!(report.succeeded, 3);
        let first = &report.results[0];
        assert_eq!(first.attempts.len(), 2);
        assert_eq!(first.attempts[0].model, "cheap");
        assert_eq!(first.attempts[1].model, "capable");
        assert_eq!(first.final_text(), Some("capable ok"));
    }

    #[test]
    fn an_exhausted_ladder_fails_the_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = mission(dir.path(), 1, vec!["cheap", "capable"]);
        let report = run_mission(&config, &runner(vec!["cheap", "capable"])).unwrap();
        assert_eq!(report.failed, 3);
        assert_eq!(report.results[0].attempts.len(), 2);
        assert!(!report.results[0].succeeded);
        assert!(matches!(
            report.results[0].stopped,
            Some(TaskStop::Ladder(StopReason::Exhausted))
        ));
    }

    #[test]
    fn the_same_failure_on_two_models_stops_the_task_early() {
        let dir = tempfile::tempdir().unwrap();
        let config = mission(dir.path(), 1, vec!["cheap", "capable", "third"]);
        let runner = runner_with_identical_failures(vec!["cheap", "capable", "third"]);
        let report = run_mission(&config, &runner).unwrap();
        assert_eq!(report.stuck, 3);
        assert_eq!(report.results[0].attempts.len(), 2);
        assert!(matches!(
            report.results[0].stopped,
            Some(TaskStop::Ladder(StopReason::Stuck { .. }))
        ));
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|model| model != "third"));
    }

    #[test]
    fn a_catalogue_ranks_candidates_by_expected_cost() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = mission(dir.path(), 1, vec!["capable", "cheap"]);
        config.catalogue = Some(catalogue());
        let runner = runner(vec!["capable"]);
        let report = run_mission(&config, &runner).unwrap();
        let first = &report.results[0];
        assert_eq!(first.attempts[0].model, "cheap");
        assert_eq!(first.final_text(), Some("cheap ok"));
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|model| model != "capable"));
    }

    #[test]
    fn a_budget_refuses_the_attempt_instead_of_running_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.catalogue = Some(catalogue());
        config.budget = Some(Budget::new(MicroUsd::from_micros(1)));
        let runner = runner(vec![]);
        let report = run_mission(&config, &runner).unwrap();
        assert_eq!(report.succeeded, 0);
        assert!(report.budget_exhausted);
        assert!(runner.calls.lock().unwrap().is_empty());
        assert!(matches!(
            report.results[0].attempts[0].outcome,
            AttemptOutcome::Skipped { .. }
        ));
        let events = read_events(&report.events_path).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::BudgetExhausted));
    }

    #[test]
    fn a_budget_without_a_catalogue_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.budget = Some(Budget::new(MicroUsd::from_micros(1)));
        assert!(run_mission(&config, &runner(vec![])).is_err());
    }

    #[test]
    fn the_mission_total_comes_from_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let entry = LedgerEntry::settle(Dispatch {
            mission: "m-1".into(),
            task: "t-1".into(),
            attempt: 1,
            model: "cheap".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(150_000),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        });
        append_entry(&ledger, &entry).unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.ledger_path = ledger;
        let report = run_mission(&config, &runner(vec![])).unwrap();
        assert_eq!(report.total_cost, MicroUsd::from_micros(150_000));
    }

    /// A crash cut a row inside a multi-byte character and the next row went
    /// onto the torn line. The strict reader failed the whole file, and the
    /// mission reported $0 spent; only the torn row may be lost.
    #[test]
    fn the_mission_total_counts_every_row_around_a_torn_line() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let charge = |task: &str| {
            LedgerEntry::settle(Dispatch {
                mission: "m-1".into(),
                task: task.into(),
                attempt: 1,
                model: "cheap".into(),
                provider: "deepseek".into(),
                usage: TokenUsage::new(1_000_000, 0, 0),
                price: ModelPrice {
                    input_miss: MicroUsd::from_micros(150_000),
                    input_hit: MicroUsd::ZERO,
                    output: MicroUsd::ZERO,
                },
                started_unix_ms: 0,
                ended_unix_ms: 1,
                outcome: Outcome::Completed,
            })
        };
        append_entry(&ledger, &charge("before")).unwrap();
        let torn =
            r#"{"mission":"m-1","task":"torn","outcome":{"rejected":{"reason":"gate “red”"}}}"#;
        let cut = torn.find('“').unwrap() + 1;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        file.write_all(&torn.as_bytes()[..cut]).unwrap();
        drop(file);
        append_entry(&ledger, &charge("after")).unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.ledger_path = ledger;
        config.catalogue = Some(catalogue());
        let report = run_mission(&config, &runner(vec![])).unwrap();
        assert_eq!(report.total_cost, MicroUsd::from_micros(300_000));
    }
}
