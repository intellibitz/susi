//! The mission scheduler.
//!
//! A mission is a set of tasks; each task is one headless turn tried against
//! the mission's candidate models, cheapest first. Up to `max_parallel` tasks
//! run at once. The gateway prices every call (so the ledger is the spend
//! record) and this module writes the mission's narrative to `events.jsonl`
//! for the cockpit.
//!
//! The scheduler depends on [`Runner`], not on OpenClaw, so a mission can be
//! tested without spawning an agent.

use crate::event::{append_event, Event, EventKind};
use crate::executor::{ExecutorConfig, RunOutcome, Runner};
use crate::ledger::read_entries;
use crate::money::MicroUsd;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
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

/// Everything a mission needs to run.
#[derive(Debug, Clone)]
pub struct MissionConfig {
    /// Mission id; also the ledger tag every call carries.
    pub id: String,
    /// Tasks to run.
    pub tasks: Vec<TaskSpec>,
    /// Candidate models, cheapest first; each failed attempt moves to the next.
    pub candidates: Vec<String>,
    /// Maximum tasks in flight at once.
    pub max_parallel: usize,
    /// Per-attempt deadline.
    pub timeout: Duration,
    /// OpenClaw executable.
    pub program: PathBuf,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Per-mission state root; each task gets `<state>/<task>/attempt-<n>`.
    pub state_dir: PathBuf,
    /// Ledger the gateway writes, read back for the mission total.
    pub ledger_path: PathBuf,
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
                | AttemptOutcome::Error { .. } => None,
            })
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
    /// Event feed written for the cockpit.
    pub events_path: PathBuf,
}

/// Run a mission to completion.
///
/// # Errors
///
/// Currently infallible beyond what the event log tolerates; the signature
/// keeps room for a mission that cannot even start (e.g. an empty candidate
/// list) without breaking callers.
pub fn run_mission(config: &MissionConfig, runner: &dyn Runner) -> EaiResult<MissionReport> {
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
            "{} task(s), {} candidate(s), {} parallel",
            config.tasks.len(),
            config.candidates.len(),
            config.max_parallel
        ),
    );

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
                let result = run_task(task, config, runner, &events);
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
        events_path,
    })
}

fn run_task(
    task: &TaskSpec,
    config: &MissionConfig,
    runner: &dyn Runner,
    events: &EventSink,
) -> TaskResult {
    let mut attempts = Vec::new();
    let mut succeeded = false;
    for (index, model) in config.candidates.iter().enumerate() {
        let attempt_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        events.emit(
            EventKind::TaskDispatched,
            &config.id,
            Some(&task.id),
            format!("{} -> {} (attempt {attempt_number})", task.id, model),
        );
        let started = Instant::now();
        let outcome = runner.run(&ExecutorConfig {
            program: config.program.clone(),
            gateway_url: config.gateway_url.clone(),
            model: model.clone(),
            workspace: task.workspace.clone(),
            state_dir: config
                .state_dir
                .join(&task.id)
                .join(format!("attempt-{index}")),
            mission: config.id.clone(),
            task: task.id.clone(),
            attempt: attempt_number,
            timeout: config.timeout,
            prompt: task.prompt.clone(),
        });
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let attempt = match outcome {
            Ok(RunOutcome::Succeeded { final_text, .. }) => {
                succeeded = true;
                AttemptResult {
                    model: model.clone(),
                    outcome: AttemptOutcome::Succeeded { final_text },
                    elapsed_ms,
                }
            }
            Ok(RunOutcome::Failed { message }) => AttemptResult {
                model: model.clone(),
                outcome: AttemptOutcome::Failed { message },
                elapsed_ms,
            },
            Ok(RunOutcome::TimedOut) => AttemptResult {
                model: model.clone(),
                outcome: AttemptOutcome::TimedOut,
                elapsed_ms,
            },
            Err(error) => AttemptResult {
                model: model.clone(),
                outcome: AttemptOutcome::Error {
                    message: error.to_string(),
                },
                elapsed_ms,
            },
        };
        events.emit(
            EventKind::TaskFinished,
            &config.id,
            Some(&task.id),
            format!(
                "{} attempt {attempt_number}: {}",
                task.id,
                describe(&attempt.outcome)
            ),
        );
        attempts.push(attempt);
        if succeeded {
            break;
        }
    }
    TaskResult {
        task: task.id.clone(),
        attempts,
        succeeded,
    }
}

fn describe(outcome: &AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Succeeded { .. } => "succeeded",
        AttemptOutcome::Failed { .. } => "failed",
        AttemptOutcome::TimedOut => "timed out",
        AttemptOutcome::Error { .. } => "executor error",
    }
}

fn mission_cost(path: &Path, mission: &str) -> MicroUsd {
    let Ok(entries) = read_entries(path) else {
        return MicroUsd::ZERO;
    };
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
    }

    impl Runner for FakeRunner {
        fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            if self.failing.contains(&config.model) {
                return Ok(RunOutcome::Failed {
                    message: format!("{} failed", config.model),
                });
            }
            Ok(RunOutcome::Succeeded {
                final_text: format!("{} ok", config.model),
                reported_cost_usd: None,
            })
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
            program: PathBuf::from("openclaw"),
            gateway_url: "http://127.0.0.1:1".into(),
            state_dir: dir.to_path_buf(),
            ledger_path: dir.join("ledger.jsonl"),
        }
    }

    #[test]
    fn a_missing_state_root_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = mission(dir.path(), 1, vec!["cheap"]);
        config.state_dir = dir.path().join("nested").join("state");
        let report = run_mission(&config, &FakeRunner { failing: vec![] }).unwrap();
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
        let report = run_mission(&config, &FakeRunner { failing: vec![] }).unwrap();
        assert_eq!(report.succeeded, 3);
        assert_eq!(report.failed, 0);
        assert!(report
            .results
            .iter()
            .all(|result| result.final_text() == Some("cheap ok")));
        let events = read_events(&report.events_path).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::MissionStarted));
        assert!(events
            .iter()
            .any(|event| event.kind == EventKind::MissionFinished));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == EventKind::TaskFinished)
                .count(),
            3
        );
    }

    #[test]
    fn a_failing_model_falls_to_the_next_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let config = mission(dir.path(), 1, vec!["cheap", "capable"]);
        let report = run_mission(
            &config,
            &FakeRunner {
                failing: vec!["cheap".into()],
            },
        )
        .unwrap();
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
        let report = run_mission(
            &config,
            &FakeRunner {
                failing: vec!["cheap".into(), "capable".into()],
            },
        )
        .unwrap();
        assert_eq!(report.failed, 3);
        assert_eq!(report.results[0].attempts.len(), 2);
        assert!(!report.results[0].succeeded);
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
        let report = run_mission(&config, &FakeRunner { failing: vec![] }).unwrap();
        assert_eq!(report.total_cost, MicroUsd::from_micros(150_000));
    }
}
