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

use crate::adapter::{land, run_task, CommandRunner, SusiTarget, SusiTaskSpec};
use crate::executor::{RunOutcome, Runner};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

/// One task in a supervisor plan.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanTask {
    /// Stable key within the plan (also the worktree suffix and state key).
    pub key: String,
    /// Task title handed to `susi tasks add`.
    pub title: String,
    /// The task's acceptance command.
    pub accept: String,
    /// Paths the task reserves.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The instruction for the agent.
    pub prompt: String,
}

/// A mission plan.
#[derive(Debug, Clone, Deserialize)]
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

    fn validate(&self) -> EaiResult<()> {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorReport {
    /// Mission id.
    pub mission: String,
    /// Task keys that landed this run.
    pub done: Vec<String>,
    /// Task keys that failed this run, with the reason.
    pub failed: Vec<(String, String)>,
    /// Task keys skipped because an earlier run landed them.
    pub skipped: Vec<String>,
}

/// Executes one planned task.
pub trait TaskExecutor {
    /// Run one task to a landing (or a failure).
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error; a task that simply did not
    /// land is [`TaskStatus::Failed`].
    fn execute(&self, task: &PlanTask) -> EaiResult<TaskStatus>;
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

/// Runs a plan, persisting after every task.
pub struct Supervisor<'a, E: TaskExecutor> {
    /// Where the resume state lives.
    pub state_path: PathBuf,
    /// How tasks are executed.
    pub executor: &'a E,
}

impl<'a, E: TaskExecutor> Supervisor<'a, E> {
    /// Build a supervisor.
    #[must_use]
    pub fn new(state_path: impl Into<PathBuf>, executor: &'a E) -> Self {
        Self {
            state_path: state_path.into(),
            executor,
        }
    }

    /// Run the plan, skipping tasks an earlier run landed.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the state cannot be read or written.
    pub fn run(&self, plan: &Plan) -> EaiResult<SupervisorReport> {
        let mut state = load_state(&self.state_path)?;
        let mut done = Vec::new();
        let mut failed = Vec::new();
        let mut skipped = Vec::new();
        for task in &plan.tasks {
            if state
                .records
                .get(&task.key)
                .is_some_and(|record| record.status == "done")
            {
                skipped.push(task.key.clone());
                continue;
            }
            let status = self
                .executor
                .execute(task)
                .unwrap_or_else(|error| TaskStatus::Failed {
                    reason: error.to_string(),
                });
            let record = match &status {
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
            state.records.insert(task.key.clone(), record);
            save_state(&self.state_path, &state)?;
            match status {
                TaskStatus::Done { .. } => done.push(task.key.clone()),
                TaskStatus::Failed { reason } => failed.push((task.key.clone(), reason)),
            }
        }
        Ok(SupervisorReport {
            mission: plan.mission.clone(),
            done,
            failed,
            skipped,
        })
    }
}

/// The production executor: one susi task through the adapter.
pub struct SusiExecutor<'a, R: CommandRunner> {
    /// The prepared target.
    pub target: &'a SusiTarget<R>,
    /// The agent runner.
    pub runner: &'a dyn Runner,
    /// Worktree name prefix.
    pub worktree_prefix: String,
    /// OpenClaw executable.
    pub program: PathBuf,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Model id.
    pub model: String,
    /// Per-turn deadline.
    pub timeout: Duration,
    /// Per-task adapter state root.
    pub state_dir: PathBuf,
}

impl<R: CommandRunner> TaskExecutor for SusiExecutor<'_, R> {
    fn execute(&self, task: &PlanTask) -> EaiResult<TaskStatus> {
        let spec = SusiTaskSpec {
            worktree_name: format!("{}-{}", self.worktree_prefix, task.key),
            title: task.title.clone(),
            accept: task.accept.clone(),
            scopes: task.scopes.clone(),
            prompt: task.prompt.clone(),
            program: self.program.clone(),
            gateway_url: self.gateway_url.clone(),
            model: self.model.clone(),
            timeout: self.timeout,
            state_dir: self.state_dir.join(&task.key),
        };
        let report = run_task(self.target, &spec, self.runner)?;
        if !report.accepted {
            return Ok(TaskStatus::Failed {
                reason: format!(
                    "agent {} / committed {} / acceptance not passed",
                    describe(&report.agent),
                    report.committed
                ),
            });
        }
        land(self.target, &report)?;
        Ok(TaskStatus::Done { task: report.task })
    }
}

fn describe(outcome: &RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Succeeded { .. } => "succeeded",
        RunOutcome::Failed { .. } => "failed",
        RunOutcome::TimedOut => "timed_out",
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
    use std::sync::Mutex;

    struct FakeExecutor {
        calls: Mutex<Vec<String>>,
        failing: Vec<String>,
    }

    impl TaskExecutor for FakeExecutor {
        fn execute(&self, task: &PlanTask) -> EaiResult<TaskStatus> {
            self.calls.lock().unwrap().push(task.key.clone());
            if self.failing.contains(&task.key) {
                Ok(TaskStatus::Failed {
                    reason: "no".into(),
                })
            } else {
                Ok(TaskStatus::Done {
                    task: format!("T-{}", task.key),
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
                    prompt: "do it".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_run_lands_every_task_and_persists_state() {
        let dir = tempfile::tempdir().unwrap();
        let executor = FakeExecutor {
            calls: Mutex::new(Vec::new()),
            failing: vec![],
        };
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
        let executor = FakeExecutor {
            calls: Mutex::new(Vec::new()),
            failing: vec![],
        };
        let supervisor = Supervisor::new(dir.path().join("state.json"), &executor);
        supervisor.run(&plan()).unwrap();
        let second = supervisor.run(&plan()).unwrap();
        assert_eq!(second.skipped, vec!["a", "b", "c"]);
        assert_eq!(executor.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn a_failed_task_is_retried_on_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let failing = FakeExecutor {
            calls: Mutex::new(Vec::new()),
            failing: vec!["b".into()],
        };
        let state = dir.path().join("state.json");
        let first = Supervisor::new(&state, &failing).run(&plan()).unwrap();
        assert_eq!(first.done, vec!["a", "c"]);
        assert_eq!(first.failed.len(), 1);

        let retry = FakeExecutor {
            calls: Mutex::new(Vec::new()),
            failing: vec![],
        };
        let second = Supervisor::new(&state, &retry).run(&plan()).unwrap();
        assert_eq!(second.done, vec!["b"]);
        assert_eq!(second.skipped, vec!["a", "c"]);
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
}
