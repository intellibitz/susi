//! The susi target adapter.
//!
//! The operations needed to run a task against susi itself: create a worktree
//! with its own worker identity, add and claim a task, run the task's declared
//! acceptance, and publish through `workflow finish`. Every operation shells
//! out to the `susi` binary; the command runner is a trait so the adapter is
//! testable without a repository.
//!
//! The adapter does not decide *what* to build — a mission does. It only makes
//! the target's own rules (worktree, claim, acceptance, finish) mechanical, so
//! an agent can be dropped into the worktree without a human driving git.

use crate::budget::{estimate_tokens, Admission, Budget, BudgetGuard, DEFAULT_OUTPUT_RESERVE};
use crate::catalog::PriceCatalogue;
use crate::executor::{ExecutorConfig, RunOutcome, Runner};
use crate::money::MicroUsd;
use crate::policy::{FailureKind, Ladder, NextAttempt, Policy, StopReason};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// One finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Exit code (`-1` when the process died without one).
    pub status: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl RunOutput {
    /// True when the command exited zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// The last lines of the failure, for an error message.
    #[must_use]
    pub fn tail(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        const LIMIT: usize = 800;
        if text.len() <= LIMIT {
            return text.trim().to_string();
        }
        text.get(text.len() - LIMIT..)
            .unwrap_or(text)
            .trim()
            .to_string()
    }
}

/// Runs a program; production uses the OS, tests use a fake.
pub trait CommandRunner: Sync {
    /// Run `program` with `args` in `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::process` when the process cannot be spawned.
    fn run(&self, program: &Path, args: &[String], cwd: &Path) -> EaiResult<RunOutput>;
}

/// The real runner.
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &Path, args: &[String], cwd: &Path) -> EaiResult<RunOutput> {
        // Redirect to files rather than pipes: `workflow start` leaves a
        // primary-checkout watcher running, and that child inherits the
        // handles. With a pipe, `Command::output()` never sees EOF and the
        // adapter hangs forever; with files, the exit status is enough.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let base = std::env::temp_dir().join(format!("susi-run-{}-{stamp}", std::process::id()));
        let stdout_path = base.with_extension("out");
        let stderr_path = base.with_extension("err");
        let stdout_file = std::fs::File::create(&stdout_path).map_err(|error| {
            EaiError::process(format!("create {}: {error}", stdout_path.display()))
        })?;
        let stderr_file = std::fs::File::create(&stderr_path).map_err(|error| {
            EaiError::process(format!("create {}: {error}", stderr_path.display()))
        })?;
        let status = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .status()
            .map_err(|error| EaiError::process(format!("run {}: {error}", program.display())))?;
        let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
        let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
        let _ = std::fs::remove_file(&stdout_path);
        let _ = std::fs::remove_file(&stderr_path);
        Ok(RunOutput {
            status: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }
}

/// A susi checkout and the binary that enforces its workflow.
pub struct SusiTarget<R: CommandRunner> {
    /// The primary checkout (worktrees are created from it).
    pub repo: PathBuf,
    /// The `susi` binary to drive.
    pub susi_program: PathBuf,
    /// How commands are run.
    pub runner: R,
}

impl SusiTarget<ProcessRunner> {
    /// A target that runs commands on this host.
    #[must_use]
    pub fn new(repo: impl Into<PathBuf>, susi_program: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            susi_program: susi_program.into(),
            runner: ProcessRunner,
        }
    }
}

impl<R: CommandRunner> SusiTarget<R> {
    fn susi(&self, args: Vec<String>, cwd: &Path) -> EaiResult<RunOutput> {
        self.runner.run(&self.susi_program, &args, cwd)
    }

    /// Create a worktree on a fresh branch and return its path.
    ///
    /// Worktree creation is serialized inside this process: `workflow start`
    /// takes a repository-level lock and fails rather than waits, so two
    /// prepares that start at the same time would lose one. The guard covers
    /// only the start; the agent runs that follow still run in parallel.
    ///
    /// # Errors
    ///
    /// Returns an error when `workflow start` fails or prints no `cd <path>`.
    pub fn start_worktree(&self, name: &str) -> EaiResult<PathBuf> {
        static WORKTREE_START: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = WORKTREE_START
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let output = self.susi(
            vec![
                "workflow".to_string(),
                "start".to_string(),
                name.to_string(),
            ],
            &self.repo,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "workflow start failed: {}",
                output.tail()
            )));
        }
        parse_worktree_path(&output.stdout).ok_or_else(|| {
            EaiError::process(format!(
                "workflow start printed no `cd <path>`: {}",
                output.tail()
            ))
        })
    }

    /// Sync a reused worktree with `origin/main`.
    ///
    /// A worktree that is reused across mission attempts can be many commits
    /// behind; the claim refuses a stale branch, so sync before claiming.
    ///
    /// # Errors
    ///
    /// Returns an error when the worktree cannot be synced (for example, it
    /// has uncommitted work from a failed attempt).
    pub fn sync(&self, worktree: &Path) -> EaiResult<()> {
        let output = self.susi(vec!["workflow".to_string(), "sync".to_string()], worktree)?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "workflow sync failed: {}",
                output.tail()
            )));
        }
        Ok(())
    }

    /// Add a task in the worktree; returns its id.
    ///
    /// # Errors
    ///
    /// Returns an error when the task cannot be added.
    pub fn add_task(&self, worktree: &Path, title: &str, accept: &str) -> EaiResult<String> {
        let output = self.susi(
            vec![
                "tasks".to_string(),
                "add".to_string(),
                title.to_string(),
                "--accept".to_string(),
                accept.to_string(),
            ],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "tasks add failed: {}",
                output.tail()
            )));
        }
        parse_added_task(&output.stdout).ok_or_else(|| {
            EaiError::process(format!("tasks add printed no task id: {}", output.tail()))
        })
    }

    /// Claim a task with its reserved scopes.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim is refused (another agent, a stale
    /// branch, overlapping scopes).
    pub fn claim(&self, worktree: &Path, id: &str, scopes: &[String]) -> EaiResult<()> {
        let mut args = vec!["tasks".to_string(), "claim".to_string(), id.to_string()];
        for scope in scopes {
            args.push("--scope".to_string());
            args.push(scope.clone());
        }
        let output = self.susi(args, worktree)?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "tasks claim failed: {}",
                output.tail()
            )));
        }
        Ok(())
    }

    /// The task's declared acceptance, split into argv.
    ///
    /// # Errors
    ///
    /// Returns an error when the task file is missing or has no `accept`.
    pub fn acceptance_argv(&self, worktree: &Path, id: &str) -> EaiResult<Vec<String>> {
        let path = task_file(worktree, id)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| EaiError::io(format!("read {}: {error}", path.display())))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| EaiError::io(format!("parse {}: {error}", path.display())))?;
        let accept = value
            .get("accept")
            .ok_or_else(|| EaiError::config(format!("task {id} declares no acceptance")))?;
        // The queue stores the parsed argv (`{"cmd": [...]}`), but a
        // hand-written task may still carry the original string.
        let argv: Vec<String> = if let Some(text) = accept.as_str() {
            text.split_whitespace().map(str::to_string).collect()
        } else if let Some(cmd) = accept.get("cmd").and_then(Value::as_array) {
            cmd.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        } else {
            return Err(EaiError::config(format!(
                "task {id} has no acceptance argv"
            )));
        };
        if argv.is_empty() {
            return Err(EaiError::config(format!(
                "task {id} declares an empty acceptance"
            )));
        }
        Ok(argv)
    }

    /// Run the task's acceptance in the worktree.
    ///
    /// # Errors
    ///
    /// Returns an error when the acceptance exits non-zero.
    pub fn accept(&self, worktree: &Path, id: &str) -> EaiResult<RunOutput> {
        let argv = self.acceptance_argv(worktree, id)?;
        let program = Path::new(&argv[0]);
        let output = self.runner.run(program, &argv[1..], worktree)?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "acceptance failed for {id}: {}",
                output.tail()
            )));
        }
        Ok(output)
    }

    /// Publish the work through `workflow finish` (gate, close, push, merge).
    ///
    /// # Errors
    ///
    /// Returns an error when finish stops for repair.
    pub fn finish(&self, worktree: &Path, id: &str) -> EaiResult<RunOutput> {
        let output = self.susi(
            vec!["workflow".to_string(), "finish".to_string(), id.to_string()],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "workflow finish failed for {id}: {}",
                output.tail()
            )));
        }
        Ok(output)
    }

    /// True when the worktree has no uncommitted changes and at least one
    /// commit ahead of `origin/main`.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot be run.
    pub fn worktree_is_committed(&self, worktree: &Path) -> EaiResult<bool> {
        let status = self.runner.run(
            Path::new("git"),
            &["status".to_string(), "--porcelain".to_string()],
            worktree,
        )?;
        if !status.success() {
            return Err(EaiError::process(format!(
                "git status failed: {}",
                status.tail()
            )));
        }
        if !status.stdout.trim().is_empty() {
            return Ok(false);
        }
        let log = self.runner.run(
            Path::new("git"),
            &[
                "log".to_string(),
                "origin/main..HEAD".to_string(),
                "--oneline".to_string(),
            ],
            worktree,
        )?;
        if !log.success() {
            return Err(EaiError::process(format!("git log failed: {}", log.tail())));
        }
        Ok(!log.stdout.trim().is_empty())
    }

    /// Commit the task record and return the commit a retry resets to.
    ///
    /// The record has to be committed before the agent runs: if the attempt is
    /// rejected, the adapter resets the worktree and runs a stronger model, and
    /// an uncommitted task record would be wiped by that reset — losing the
    /// claim's own record and leaving an orphan worktree.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot stage or commit the record.
    pub fn commit_task_record(&self, worktree: &Path, id: &str) -> EaiResult<String> {
        let path = format!(".agents/tasks/{id}.json");
        let add = self.git(worktree, &["add", "--", &path])?;
        if !add.success() {
            return Err(EaiError::process(format!(
                "git add {path} failed: {}",
                add.tail()
            )));
        }
        let message = format!("Record {id}\n\nTask: {id}");
        let commit = self.git(worktree, &["commit", "-m", &message])?;
        if !commit.success() {
            return Err(EaiError::process(format!(
                "git commit of {path} failed: {}",
                commit.tail()
            )));
        }
        let head = self.git(worktree, &["rev-parse", "HEAD"])?;
        if !head.success() {
            return Err(EaiError::process(format!(
                "git rev-parse failed: {}",
                head.tail()
            )));
        }
        Ok(head.stdout.trim().to_string())
    }

    /// Discard everything a rejected attempt wrote, keeping the task record.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot reset the worktree.
    pub fn reset_to(&self, worktree: &Path, commit: &str) -> EaiResult<()> {
        let reset = self.git(worktree, &["reset", "--hard", commit])?;
        if !reset.success() {
            return Err(EaiError::process(format!(
                "git reset --hard {commit} failed: {}",
                reset.tail()
            )));
        }
        let clean = self.git(worktree, &["clean", "-fd", "-e", ".agents/tasks"])?;
        if !clean.success() {
            return Err(EaiError::process(format!(
                "git clean failed: {}",
                clean.tail()
            )));
        }
        Ok(())
    }

    fn git(&self, worktree: &Path, args: &[&str]) -> EaiResult<RunOutput> {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.runner.run(Path::new("git"), &argv, worktree)
    }
}

/// How a prepared run is bounded and escalated.
pub struct SusiRunPolicy<'a> {
    /// Price catalogue used to rank alternates and estimate spend.
    pub catalogue: Option<&'a PriceCatalogue>,
    /// Shared budget guard, when the mission has a cap.
    pub guard: Option<&'a BudgetGuard>,
    /// Ledger the gateway writes (spend authority).
    pub ledger_path: &'a Path,
    /// Mission id every attempt is billed to.
    pub mission: &'a str,
    /// Escalation policy.
    pub policy: Policy,
}

impl SusiRunPolicy<'_> {
    /// A run with no budget and no catalogue: the task's model is the only one.
    #[must_use]
    pub fn unmetered() -> SusiRunPolicy<'static> {
        SusiRunPolicy {
            catalogue: None,
            guard: None,
            ledger_path: Path::new(""),
            mission: "",
            policy: Policy::default(),
        }
    }
}

/// What an agent should do in a prepared susi worktree.
pub struct SusiTaskSpec {
    /// Worktree/branch name (also the mission tag on the ledger).
    pub worktree_name: String,
    /// Task title.
    pub title: String,
    /// The task's acceptance command.
    pub accept: String,
    /// Paths the task reserves.
    pub scopes: Vec<String>,
    /// The instruction for the agent (the task id and trailer are appended).
    pub prompt: String,
    /// OpenClaw executable.
    pub program: PathBuf,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Model id the task prefers.
    pub model: String,
    /// Models to escalate to when the preferred one does not pass.
    pub alternates: Vec<String>,
    /// Per-turn deadline.
    pub timeout: Duration,
    /// Adapter state root.
    pub state_dir: PathBuf,
}

/// One model attempt inside a prepared task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SusiAttemptRecord {
    /// Model tried.
    pub model: String,
    /// Whether the agent left a clean commit.
    pub committed: bool,
    /// Whether the declared acceptance passed.
    pub accepted: bool,
    /// The agent's own verdict, in the operator's language.
    pub outcome: String,
    /// Estimated cost of the attempt, when it could be priced.
    pub estimate: Option<MicroUsd>,
}

/// What one prepared run produced.
pub struct SusiRunReport {
    /// The worktree the agent worked in.
    pub worktree: PathBuf,
    /// The task id the adapter added and claimed.
    pub task: String,
    /// The model of the accepted attempt (or the last one tried).
    pub model: String,
    /// The last agent outcome.
    pub agent: RunOutcome,
    /// Whether the accepted attempt left a clean commit ahead of `origin/main`.
    pub committed: bool,
    /// Whether the declared acceptance passed.
    pub accepted: bool,
    /// Every attempt, in order.
    pub attempts: Vec<SusiAttemptRecord>,
    /// True when the ladder stopped because the budget would be crossed.
    pub budget_exhausted: bool,
}

/// Prepare a worktree, add and claim a task, run the agent in it, and verify.
///
/// The agent is told the task id and the trailer it must commit with; it is
/// told not to run `susi` itself, because the adapter already owns the claim.
///
/// When an attempt does not pass, the worktree is reset to the task record and
/// the next model on the ladder runs. A rejected attempt is never published,
/// and a repeated failure across models stops the task instead of spending the
/// rest of the budget on the same wall.
///
/// # Errors
///
/// Returns an error when the worktree, task, claim or agent cannot be started.
pub fn run_task<R: CommandRunner>(
    target: &SusiTarget<R>,
    spec: &SusiTaskSpec,
    runner: &dyn Runner,
    policy: &SusiRunPolicy<'_>,
) -> EaiResult<SusiRunReport> {
    let worktree = target.start_worktree(&spec.worktree_name)?;
    target.sync(&worktree)?;
    let task = target.add_task(&worktree, &spec.title, &spec.accept)?;
    target.claim(&worktree, &task, &spec.scopes)?;
    let base = target.commit_task_record(&worktree, &task)?;
    let ranked = rank_alternates(spec, policy)?;
    let prompt = format!(
        "Work in this order: read only what you need to identify the exact API you are changing, \
then make the edit. Your first write must happen before you have read more than ten files; do \
not survey the repository. After the first write, run the task's acceptance command and iterate \
on real output.\n\n{}\n\nTask id: {task}. When the work is done, run `git add -A` and commit with a message whose last line is exactly `Task: {task}`. Do not run any `susi` command, do not push, and change nothing outside the task.",
        spec.prompt
    );
    let mut ladder = Ladder::new(ranked, policy.policy);
    let mut attempts = Vec::new();
    let mut last_outcome = None;
    let mut last_model = spec.model.clone();
    let mut committed = false;
    let mut accepted = false;
    let mut budget_exhausted = false;
    loop {
        let attempt = match ladder.next_attempt() {
            NextAttempt::Attempt(attempt) => attempt,
            NextAttempt::Stop(reason) => {
                if attempts.is_empty() {
                    last_outcome = Some(RunOutcome::Failed {
                        message: describe_stop(&reason),
                    });
                }
                break;
            }
        };
        last_model = attempt.model.clone();
        let estimate = policy
            .catalogue
            .and_then(|catalogue| catalogue.price(&attempt.model))
            .map(|price| estimate_tokens(price, &prompt, DEFAULT_OUTPUT_RESERVE));
        if let (Some(guard), Some(estimate)) = (policy.guard, estimate) {
            let spent = Budget::spent(policy.ledger_path, policy.mission);
            if let Admission::Exhausted { .. } = guard.admit(spent, estimate) {
                last_outcome = Some(RunOutcome::Failed {
                    message: format!(
                        "budget: spent {} of {}; attempt not dispatched",
                        spent.total,
                        guard.budget().limit
                    ),
                });
                budget_exhausted = true;
                break;
            }
        }
        let agent = runner.run(&ExecutorConfig {
            program: spec.program.clone(),
            gateway_url: spec.gateway_url.clone(),
            model: attempt.model.clone(),
            workspace: worktree.clone(),
            state_dir: spec
                .state_dir
                .join(&task)
                .join(format!("attempt-{}", attempt.number)),
            mission: policy.mission.to_string(),
            task: task.clone(),
            attempt: attempt.number,
            timeout: spec.timeout,
            prompt: prompt.clone(),
        })?;
        let attempt_committed = if matches!(&agent, RunOutcome::Succeeded { .. }) {
            target.worktree_is_committed(&worktree)?
        } else {
            false
        };
        let attempt_accepted = attempt_committed && target.accept(&worktree, &task).is_ok();
        if let (Some(guard), Some(estimate)) = (policy.guard, estimate) {
            let spent = Budget::spent(policy.ledger_path, policy.mission);
            guard.settle(spent, estimate);
        }
        attempts.push(SusiAttemptRecord {
            model: attempt.model.clone(),
            committed: attempt_committed,
            accepted: attempt_accepted,
            outcome: describe_outcome(&agent),
            estimate,
        });
        if attempt_accepted {
            committed = attempt_committed;
            accepted = true;
            last_outcome = Some(agent);
            break;
        }
        let (kind, message) = failure_of(&agent);
        ladder.record(&attempt.model, kind, &message);
        // A rejected attempt is discarded: the next model starts from the
        // task record, not from work that already failed acceptance.
        target.reset_to(&worktree, &base)?;
        last_outcome = Some(agent);
    }
    Ok(SusiRunReport {
        worktree,
        task,
        model: last_model,
        agent: last_outcome.unwrap_or(RunOutcome::Failed {
            message: "the ladder stopped before any attempt ran".to_string(),
        }),
        committed,
        accepted,
        attempts,
        budget_exhausted,
    })
}

/// The model order for a task: its own choice first, then the ranked rest.
fn rank_alternates(spec: &SusiTaskSpec, policy: &SusiRunPolicy<'_>) -> EaiResult<Vec<String>> {
    let mut models = vec![spec.model.clone()];
    for model in &spec.alternates {
        if !models.contains(model) {
            models.push(model.clone());
        }
    }
    let mut ranked = match policy.catalogue {
        Some(catalogue) => {
            let entries = crate::ledger::read_entries(policy.ledger_path).unwrap_or_default();
            crate::policy::rank_models(
                catalogue,
                &models,
                crate::policy::UsageProfile::agent(),
                &crate::policy::history(&entries),
            )?
            .into_iter()
            .map(|candidate| candidate.model)
            .collect::<Vec<_>>()
        }
        None => models.clone(),
    };
    // The task's own model is a deliberate assignment; keep it first, then the
    // ranked alternates.
    if let Some(position) = ranked.iter().position(|model| model == &spec.model) {
        let preferred = ranked.remove(position);
        ranked.insert(0, preferred);
    }
    Ok(ranked)
}

fn failure_of(agent: &RunOutcome) -> (FailureKind, String) {
    match agent {
        RunOutcome::Succeeded { .. } => (FailureKind::Semantic, "accepted".to_string()),
        RunOutcome::Failed { message } => (FailureKind::classify(message), message.clone()),
        RunOutcome::TimedOut => (FailureKind::Timeout, "agent timed out".to_string()),
    }
}

fn describe_outcome(agent: &RunOutcome) -> String {
    match agent {
        RunOutcome::Succeeded { .. } => "succeeded".to_string(),
        RunOutcome::Failed { message } => format!("failed: {message}"),
        RunOutcome::TimedOut => "timed out".to_string(),
    }
}

fn describe_stop(reason: &StopReason) -> String {
    match reason {
        StopReason::Exhausted => "the attempt cap was reached".to_string(),
        StopReason::Stuck { signature } => format!("stopped after the same failure: {signature}"),
    }
}

/// Publish an accepted run through `workflow finish`.
///
/// # Errors
///
/// Returns an error when finish stops for repair.
pub fn land<R: CommandRunner>(
    target: &SusiTarget<R>,
    report: &SusiRunReport,
) -> EaiResult<RunOutput> {
    target.finish(&report.worktree, &report.task)
}

fn task_file(worktree: &Path, id: &str) -> EaiResult<PathBuf> {
    let open = worktree
        .join(".agents")
        .join("tasks")
        .join(format!("{id}.json"));
    if open.is_file() {
        return Ok(open);
    }
    let done = worktree
        .join(".agents")
        .join("tasks")
        .join("done")
        .join(format!("{id}.json"));
    if done.is_file() {
        return Ok(done);
    }
    Err(EaiError::io(format!(
        "task {id} not found under {}",
        worktree.display()
    )))
}

/// The last `cd <path>` line `workflow start` prints.
fn parse_worktree_path(stdout: &str) -> Option<PathBuf> {
    stdout.lines().rev().find_map(|line| {
        line.strip_prefix("cd ")
            .map(|path| PathBuf::from(path.trim()))
    })
}

/// The id from `added T-… — commit …`.
fn parse_added_task(stdout: &str) -> Option<String> {
    let marker = "added ";
    let start = stdout.find(marker)? + marker.len();
    let id: String = stdout[start..]
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
        })
        .collect();
    id.starts_with("T-").then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    type Call = (PathBuf, Vec<String>, PathBuf);

    struct FakeRunner {
        calls: Mutex<Vec<Call>>,
        replies: Mutex<VecDeque<RunOutput>>,
    }

    impl FakeRunner {
        fn with(replies: Vec<RunOutput>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                replies: Mutex::new(replies.into()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &Path, args: &[String], cwd: &Path) -> EaiResult<RunOutput> {
            self.calls.lock().unwrap().push((
                program.to_path_buf(),
                args.to_vec(),
                cwd.to_path_buf(),
            ));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(RunOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                }))
        }
    }

    fn ok(stdout: &str) -> RunOutput {
        RunOutput {
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn target(replies: Vec<RunOutput>) -> SusiTarget<FakeRunner> {
        SusiTarget {
            repo: PathBuf::from("repo"),
            susi_program: PathBuf::from("susi"),
            runner: FakeRunner::with(replies),
        }
    }

    #[test]
    fn start_worktree_reads_the_last_cd_line() {
        let target = target(vec![ok("fetching…\nworktree: x\ncd E:/src/wt\n")]);
        let path = target.start_worktree("demo").unwrap();
        assert_eq!(path, PathBuf::from("E:/src/wt"));
        let calls = target.runner.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            vec![
                "workflow".to_string(),
                "start".to_string(),
                "demo".to_string()
            ]
        );
    }

    #[test]
    fn add_task_returns_the_id_and_claim_reserves_scopes() {
        let target = target(vec![ok("added T-AGENT-7 — commit it\n")]);
        let worktree = PathBuf::from("wt");
        let id = target.add_task(&worktree, "docs", "cargo test").unwrap();
        assert_eq!(id, "T-AGENT-7");
        target
            .claim(
                &worktree,
                &id,
                &["src/cli".to_string(), "Cargo.toml".to_string()],
            )
            .unwrap();
        let calls = target.runner.calls.lock().unwrap();
        assert_eq!(
            calls[1].1,
            vec![
                "tasks".to_string(),
                "claim".to_string(),
                "T-AGENT-7".to_string(),
                "--scope".to_string(),
                "src/cli".to_string(),
                "--scope".to_string(),
                "Cargo.toml".to_string()
            ]
        );
    }

    #[test]
    fn acceptance_is_read_from_the_task_file_and_run_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".agents").join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("T-1.json"),
            r#"{"id":"T-1","accept":"cargo test -p susi-harness"}"#,
        )
        .unwrap();
        let target = target(vec![ok("tests pass")]);
        let argv = target.acceptance_argv(dir.path(), "T-1").unwrap();
        assert_eq!(argv, vec!["cargo", "test", "-p", "susi-harness"]);
        let output = target.accept(dir.path(), "T-1").unwrap();
        assert_eq!(output.stdout, "tests pass");
        let calls = target.runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, PathBuf::from("cargo"));
        assert_eq!(
            calls[0].1,
            vec![
                "test".to_string(),
                "-p".to_string(),
                "susi-harness".to_string()
            ]
        );
        assert_eq!(calls[0].2, dir.path());
    }

    #[test]
    fn acceptance_argv_is_read_from_the_queue_shape() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".agents").join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("T-2.json"),
            r#"{"accept":{"cmd":["cargo","test","-p","susi-harness"]}}"#,
        )
        .unwrap();
        let argv = target(vec![]).acceptance_argv(dir.path(), "T-2").unwrap();
        assert_eq!(argv, vec!["cargo", "test", "-p", "susi-harness"]);
    }

    #[test]
    fn a_failed_command_is_an_error_with_its_tail() {
        let target = target(vec![RunOutput {
            status: 1,
            stdout: String::new(),
            stderr: "no such task".to_string(),
        }]);
        let error = target.start_worktree("demo").unwrap_err();
        assert!(error.to_string().contains("no such task"));
    }

    struct FakeExec {
        outcome: RunOutcome,
        prompts: Mutex<Vec<String>>,
    }

    impl Runner for FakeExec {
        fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            self.prompts.lock().unwrap().push(config.prompt.clone());
            Ok(self.outcome.clone())
        }
    }

    #[test]
    fn a_prepared_run_claims_commits_accepts_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".agents").join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("T-AGENT-1.json"),
            r#"{"accept":"cargo test -p susi-harness"}"#,
        )
        .unwrap();
        let target = target(vec![
            ok(&format!("cd {}\n", dir.path().display())),
            ok(""),
            ok("added T-AGENT-1 — commit\n"),
            ok(""),
            ok(""),
            ok(""),
            ok("base123\n"),
            ok(""),
            ok("abc123 docs\n"),
            ok(""),
        ]);
        let exec = FakeExec {
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            prompts: Mutex::new(Vec::new()),
        };
        let spec = SusiTaskSpec {
            worktree_name: "smoke".into(),
            title: "docs".into(),
            accept: "cargo test -p susi-harness".into(),
            scopes: vec!["docs/harness.md".into()],
            prompt: "write docs".into(),
            program: PathBuf::from("openclaw"),
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert_eq!(report.task, "T-AGENT-1");
        assert!(report.committed);
        assert!(report.accepted);
        assert_eq!(report.attempts.len(), 1);
        let prompts = exec.prompts.lock().unwrap();
        assert!(prompts[0].contains("Task: T-AGENT-1"));
        assert!(prompts[0].contains("before you have read more than ten files"));
        assert!(prompts[0].contains("write docs"));
    }

    #[test]
    fn an_agent_failure_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(vec![
            ok(&format!("cd {}\n", dir.path().display())),
            ok(""),
            ok("added T-AGENT-2 — commit\n"),
            ok(""),
            ok(""),
            ok(""),
            ok("base123\n"),
            ok(""),
            ok(""),
        ]);
        let exec = FakeExec {
            outcome: RunOutcome::Failed {
                message: "nope".into(),
            },
            prompts: Mutex::new(Vec::new()),
        };
        let spec = SusiTaskSpec {
            worktree_name: "smoke".into(),
            title: "docs".into(),
            accept: "cargo test -p susi-harness".into(),
            scopes: vec!["docs/harness.md".into()],
            prompt: "write docs".into(),
            program: PathBuf::from("openclaw"),
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert_eq!(report.task, "T-AGENT-2");
        assert!(!report.committed);
        assert!(!report.accepted);
        assert_eq!(report.attempts.len(), 1);
    }

    #[test]
    fn a_missing_acceptance_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(target(vec![]).acceptance_argv(dir.path(), "T-404").is_err());
    }
}
