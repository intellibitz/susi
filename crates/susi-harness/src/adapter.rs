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
use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
use crate::money::MicroUsd;
use crate::policy::{FailureKind, Ladder, NextAttempt, Policy, StopReason};
use crate::pricing::TokenUsage;
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
        let start = |target: &Self| {
            target.susi(
                vec![
                    "workflow".to_string(),
                    "start".to_string(),
                    name.to_string(),
                ],
                &target.repo,
            )
        };
        let mut output = start(self)?;
        if !output.success() && Self::looks_like_leftover(&output.tail()) {
            // A killed mission can leave its directory or branch behind, and
            // both make `workflow start` refuse the next attempt. Clear the
            // leftover once, then retry.
            self.clear_leftover(name);
            output = start(self)?;
        }
        if !output.success() {
            return Err(EaiError::process(format!(
                "workflow start failed: {}",
                output.tail()
            )));
        }
        let worktree = parse_worktree_path(&output.stdout).ok_or_else(|| {
            EaiError::process(format!(
                "workflow start printed no `cd <path>`: {}",
                output.tail()
            ))
        })?;
        // A worktree keeps a private `target/` unless it is linked to the
        // primary's, so every mission would compile the whole dependency graph
        // cold and one acceptance command can block a worker for half an hour.
        // The linker is idempotent, never fails a caller, and leaves alone a
        // target that already holds a build.
        let linker = self.repo.join("scripts").join("link-shared-target.sh");
        if linker.is_file() {
            let _ = self.runner.run(
                Path::new("sh"),
                &[
                    linker.to_string_lossy().to_string(),
                    worktree.to_string_lossy().to_string(),
                ],
                &self.repo,
            );
        }
        Ok(worktree)
    }

    /// True when `workflow start` refused because a killed mission left its
    /// directory or branch behind.
    fn looks_like_leftover(tail: &str) -> bool {
        tail.contains("refusing to reuse") || tail.contains("already exists")
    }

    /// Remove what a killed mission left behind: its worktree directory, the
    /// stale registration and the harness branch with the same name. Only called
    /// after `workflow start` refused, so a healthy worktree is never touched.
    fn clear_leftover(&self, name: &str) {
        let Some(parent) = self.repo.parent() else {
            return;
        };
        let Some(dir) = self.repo.file_name() else {
            return;
        };
        let dir = dir.to_string_lossy();
        for candidate in [format!("{dir}-{name}"), format!("{dir}-harness-{name}")] {
            let path = parent.join(candidate);
            let _ = self.git(
                &self.repo,
                &["worktree", "remove", "--force", &path.to_string_lossy()],
            );
            if path.exists() {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        let _ = self.git(&self.repo, &["worktree", "prune"]);
        let _ = self.git(&self.repo, &["branch", "-D", name]);
        let _ = self.git(&self.repo, &["branch", "-D", &format!("harness-{name}")]);
    }

    /// Free the claim when a mission could not finish. A failed ladder must not
    /// leave the task owned by a dead attempt: the next model, mission or worker
    /// has to be able to claim it without a manual forced release.
    ///
    /// # Errors
    ///
    /// Returns an error when the release itself fails.
    pub fn release(&self, worktree: &Path, id: &str) -> EaiResult<RunOutput> {
        self.susi(
            vec![
                "tasks".to_string(),
                "release".to_string(),
                id.to_string(),
                "--force".to_string(),
            ],
            worktree,
        )
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
        // A test filter that matches nothing exits zero, which proves nothing:
        // it let the precheck treat unimplemented GMCP work as already delivered.
        if Self::is_test_command(&argv) && Self::ran_zero_tests(&output.stdout) {
            return Err(EaiError::process(format!(
                "acceptance for {id} ran zero tests — a filter that matches nothing proves nothing: {}",
                output.tail()
            )));
        }
        Ok(output)
    }

    /// True when the acceptance argv runs a test command whose passed count
    /// decides whether anything was proven.
    fn is_test_command(argv: &[String]) -> bool {
        let Some(program) = argv.first() else {
            return false;
        };
        let name = Path::new(program)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default();
        name == "cargo" && matches!(argv.get(1).map(String::as_str), Some("test" | "nextest"))
    }

    /// True only when the output carries libtest summaries and every one reports
    /// zero passed tests. An output with no summary at all is a non-libtest
    /// checker, which this guard does not judge.
    fn ran_zero_tests(text: &str) -> bool {
        let counts: Vec<u64> = text
            .lines()
            .filter_map(|line| line.split("test result: ok.").nth(1))
            .filter_map(|rest| rest.split(" passed").next())
            .filter_map(|count| count.trim().parse::<u64>().ok())
            .collect();
        !counts.is_empty() && counts.iter().all(|passed| *passed == 0)
    }

    /// Close the task when its acceptance already passes on the untouched base.
    ///
    /// # Errors
    ///
    /// Returns an error when the close is refused (for example, no live claim).
    pub fn close(&self, worktree: &Path, id: &str) -> EaiResult<RunOutput> {
        self.susi(
            vec!["tasks".to_string(), "close".to_string(), id.to_string()],
            worktree,
        )
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

    /// True when `base` is still an ancestor of HEAD.
    ///
    /// A worker must never rewrite the branch's history; if it did, the base
    /// commit it was given is no longer in the line it claims to extend.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot be run.
    pub fn base_is_ancestor(&self, worktree: &Path, base: &str) -> EaiResult<bool> {
        let probe = self.git(worktree, &["merge-base", "--is-ancestor", base, "HEAD"])?;
        Ok(probe.success())
    }

    /// The worktree's uncommitted changes, as `git status --porcelain`, so an
    /// attempt that leaves nothing behind is a recorded fact.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot be run.
    pub fn git_status(&self, worktree: &Path) -> EaiResult<String> {
        let status = self.git(worktree, &["status", "--porcelain"])?;
        Ok(status.stdout.clone())
    }

    /// Stage and commit whatever the worker left in the working tree.
    ///
    /// The adapter owns git, not the worker: a worker that runs `git reset` or
    /// `git commit --amend` can drop history it was handed, and no prompt can
    /// make that safe. The worker edits files; the commit, its trailer and its
    /// exact task are the adapter's.
    ///
    /// Returns `false` when there was nothing to commit.
    ///
    /// # Errors
    ///
    /// Returns an error when staging or committing fails.
    pub fn commit_work(&self, worktree: &Path, id: &str) -> EaiResult<bool> {
        // The worker edits text without running rustfmt, and the repo's
        // pre-commit hook rejects unformatted code. Format before staging so
        // the commit carries the formatter's own output instead of discarding
        // correct work on a hook.
        let fmt = self.runner.run(
            Path::new("cargo"),
            &["fmt".to_string(), "--all".to_string()],
            worktree,
        )?;
        if !fmt.success() {
            return Err(EaiError::process(format!(
                "cargo fmt --all failed before commit: {}",
                fmt.tail()
            )));
        }
        let add = self.git(worktree, &["add", "-A"])?;
        if !add.success() {
            return Err(EaiError::process(format!(
                "git add -A failed: {}",
                add.tail()
            )));
        }
        let status = self.git(worktree, &["status", "--porcelain"])?;
        if !status.success() {
            return Err(EaiError::process(format!(
                "git status failed: {}",
                status.tail()
            )));
        }
        if status.stdout.trim().is_empty() {
            return Ok(false);
        }
        let message = format!("Complete {id}\n\nTask: {id}");
        let commit = self.git(worktree, &["commit", "-m", &message])?;
        if !commit.success() {
            return Err(EaiError::process(format!(
                "git commit of the worker's changes failed: {}",
                commit.tail()
            )));
        }
        Ok(true)
    }

    /// Run the repository's compile-and-lint gate for the packages the
    /// task's scopes touch, so a change that would fail `workflow finish`
    /// is rejected while the attempt can still be retried cheaply.
    ///
    /// # Errors
    ///
    /// Returns an error when cargo cannot be run.
    pub fn gate(&self, worktree: &Path, scopes: &[String]) -> EaiResult<RunOutput> {
        let mut packages: Vec<String> = Vec::new();
        for scope in scopes {
            let package = if let Some(rest) = scope.strip_prefix("crates/") {
                rest.split('/').next().unwrap_or_default().to_string()
            } else if scope.starts_with("xtask/") {
                "xtask".to_string()
            } else {
                "susi".to_string()
            };
            if !package.is_empty() && !packages.contains(&package) {
                packages.push(package);
            }
        }
        // Worktrees share the primary's target directory, so an artifact built
        // from another checkout can shadow this worktree's sources and make the
        // gate fail on code it never read. Drop the touched packages' artifacts
        // first so the gate compiles what is actually here.
        for package in &packages {
            let _ = self.runner.run(
                Path::new("cargo"),
                &["clean".to_string(), "-p".to_string(), package.clone()],
                worktree,
            );
        }
        let mut args: Vec<String> = vec!["clippy".to_string()];
        for package in &packages {
            args.push("-p".to_string());
            args.push(package.clone());
        }
        args.extend(
            ["--all-targets", "--locked", "--", "-D", "warnings"]
                .iter()
                .map(|arg| (*arg).to_string()),
        );
        self.runner.run(Path::new("cargo"), &args, worktree)
    }

    /// True when HEAD carries a commit after `base` and the tree is clean.
    ///
    /// The adapter commits the task record before the agent runs, so a simple
    /// "commits ahead of `origin/main`" check is true even when the agent wrote
    /// nothing — and the declared acceptance (often an unrelated crate's test)
    /// would then pass on unchanged code. Work means a commit *after the
    /// record*, not the record itself.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot be run.
    pub fn worktree_has_work_after(&self, worktree: &Path, base: &str) -> EaiResult<bool> {
        if base.trim().is_empty() {
            return Ok(false);
        }
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
        let range = format!("{base}..HEAD");
        let log = self.runner.run(
            Path::new("git"),
            &["log".to_string(), "--oneline".to_string(), range],
            worktree,
        )?;
        if !log.success() {
            return Err(EaiError::process(format!("git log failed: {}", log.tail())));
        }
        Ok(!log.stdout.trim().is_empty())
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

    /// The worktree's current head commit.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot be run or cannot resolve HEAD.
    pub fn head(&self, worktree: &Path) -> EaiResult<String> {
        let head = self.git(worktree, &["rev-parse", "HEAD"])?;
        if !head.success() {
            return Err(EaiError::process(format!(
                "git rev-parse HEAD failed: {}",
                head.tail()
            )));
        }
        Ok(head.stdout.trim().to_string())
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
    /// An existing queue task to claim instead of adding one.
    pub task: Option<String>,
    /// Close a task whose acceptance already passes on the untouched base
    /// instead of spending a worker ladder on work that cannot be committed.
    pub precheck: bool,
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
    // An existing task already has its record on the branch: claim it and keep
    // the synced head as the base a rejected attempt resets to. A new task is
    // minted here and its record is committed first, for the same reason.
    let (task, base) = match &spec.task {
        Some(existing) => {
            target.claim(&worktree, existing, &spec.scopes)?;
            (existing.clone(), target.head(&worktree)?)
        }
        None => {
            let task = target.add_task(&worktree, &spec.title, &spec.accept)?;
            target.claim(&worktree, &task, &spec.scopes)?;
            let base = target.commit_task_record(&worktree, &task)?;
            (task, base)
        }
    };
    // The task's definition of done may already hold on the untouched base: a
    // worker ladder on it can only end in an empty commit. Check before spending
    // a model call, and publish the close when it does.
    if spec.precheck
        && !spec.accept.trim().is_empty()
        && target.accept(&worktree, &task).is_ok()
        && target.git_status(&worktree)?.trim().is_empty()
    {
        target.close(&worktree, &task)?;
        let closed_committed = target.commit_work(&worktree, &task).unwrap_or(false);
        let finished = target.finish(&worktree, &task);
        if finished.is_err() {
            // The precheck could not publish: the claim must not outlive this
            // attempt, exactly as when a ladder exhausts its attempts.
            let _ = target.release(&worktree, &task);
        }
        return Ok(SusiRunReport {
            worktree,
            task,
            model: "precheck".to_string(),
            agent: RunOutcome::Succeeded {
                final_text: "the task's acceptance already passes on the base; closed as delivered"
                    .to_string(),
                reported_cost_usd: None,
            },
            committed: closed_committed,
            accepted: finished.is_ok(),
            attempts: Vec::new(),
            budget_exhausted: false,
        });
    }
    let ranked = rank_alternates(spec, policy);
    let base_prompt = format!(
        "Work in this order: read only what you need to identify the exact API you are changing, \
then make the edit. Your first write must happen before you have read more than ten files; do \
not survey the repository. After the first write, run the task's acceptance command and iterate \
on real output.\n\n{}\n\nTask id: {task}. Leave the finished changes in the working tree. Do not \
run any git command: the harness stages and commits them for you, with the task trailer. Do not \
reset, rebase, amend or move a commit, do not run any `susi` command, do not push, and change \
nothing outside the task.",
        spec.prompt
    );
    let mut prompt = base_prompt.clone();
    // Each model's context window, so a prompt that overflows one goes next
    // to a model that can hold it.
    let registry = crate::providers::Registry::builtin();
    let contexts = ranked
        .iter()
        .filter_map(|model| {
            let window = registry.resolve(model)?.capabilities.context?;
            Some((model.clone(), window))
        })
        .collect();
    let mut ladder = Ladder::new(ranked, policy.policy).with_contexts(contexts);
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
        let (attempt_committed, history_rewritten) =
            if matches!(&agent, RunOutcome::Succeeded { .. }) {
                if target.base_is_ancestor(&worktree, &base)? {
                    let committed_now = target.commit_work(&worktree, &task)?;
                    (
                        committed_now && target.worktree_has_work_after(&worktree, &base)?,
                        false,
                    )
                } else {
                    (false, true)
                }
            } else {
                (false, false)
            };
        // The acceptance command proves the behaviour; the repository's
        // compile-and-lint gate proves the change is publishable. Run it
        // now, while another attempt is still cheap, and hand its output to
        // the next attempt instead of letting `workflow finish` reject the
        // work after the ladder has already stopped.
        let mut gate_failure: Option<String> = None;
        let gate_ok = if attempt_committed && !history_rewritten {
            let gate = target.gate(&worktree, &spec.scopes)?;
            if gate.success() {
                true
            } else {
                gate_failure = Some(gate.tail());
                false
            }
        } else {
            false
        };
        // Record what the attempt left behind and what the acceptance said, so a
        // failure is diagnosable from the adapter state instead of a transcript.
        let attempt_state = spec
            .state_dir
            .join(&task)
            .join(format!("attempt-{}", attempt.number));
        let _ = std::fs::create_dir_all(&attempt_state);
        if let Ok(status) = target.git_status(&worktree) {
            let _ = std::fs::write(attempt_state.join("git-status.txt"), status);
        }
        let acceptance = target.accept(&worktree, &task);
        match &acceptance {
            Ok(output) => {
                let _ = std::fs::write(
                    attempt_state.join("acceptance.txt"),
                    format!("status: {}\n{}", output.status, output.tail()),
                );
            }
            Err(error) => {
                let _ = std::fs::write(
                    attempt_state.join("acceptance.txt"),
                    format!("error: {error}"),
                );
            }
        }
        let attempt_accepted =
            attempt_committed && gate_ok && !history_rewritten && acceptance.is_ok();
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
            record_verdict(
                policy,
                &task,
                attempt.number,
                &attempt.model,
                Outcome::Accepted,
            );
            committed = attempt_committed;
            accepted = true;
            last_outcome = Some(agent);
            break;
        }
        let (kind, message) = if history_rewritten {
            (
                FailureKind::Semantic,
                "worker rewrote the branch history it was given".to_string(),
            )
        } else if let Some(tail) = gate_failure.take() {
            (
                FailureKind::Semantic,
                format!("quality gate failed: {tail}"),
            )
        } else {
            failure_of(&agent)
        };
        record_verdict(
            policy,
            &task,
            attempt.number,
            &attempt.model,
            Outcome::Rejected {
                reason: message.chars().take(240).collect(),
            },
        );
        ladder.record(&attempt.model, kind, &message);
        // Keep a rejected attempt's work for the next model to finish; only a
        // rewritten history makes the base unusable. The next prompt names the
        // acceptance or gate failure to fix.
        if history_rewritten {
            target.reset_to(&worktree, &base)?;
        }
        prompt = format!(
            "{base_prompt}\n\nThe previous attempt was rejected by the repository's quality gate. \
Fix this in the worktree before you finish:\n{message}"
        );
        last_outcome = Some(agent);
    }
    if !accepted {
        // The worktree stays for diagnosis, but the claim must not outlive the
        // attempt that could not publish it.
        let _ = target.release(&worktree, &task);
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

/// The model order for a task: its own choice first, then the fleet's ladder
/// in the fleet's order, with unreliable models and cooling providers moved
/// back ([`crate::policy::arrange`]). The ladder already ranks expiring
/// credit, cache, price and capability; re-sorting it by price here would
/// undo all of that.
fn rank_alternates(spec: &SusiTaskSpec, policy: &SusiRunPolicy<'_>) -> Vec<String> {
    let entries = crate::ledger::read_entries(policy.ledger_path).unwrap_or_default();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let registry = crate::providers::Registry::builtin();
    let provider_of = |model: &str| registry.resolve(model).map(|resolved| resolved.provider);
    crate::policy::arrange(
        &spec.model,
        &spec.alternates,
        &crate::policy::Standing {
            history: &crate::policy::history(&entries),
            cooling: &crate::policy::cooling_providers(&entries, now_ms),
            failed: &crate::policy::failed_models(&entries),
        },
        &provider_of,
    )
}

/// Record how an attempt ended, so the ranking learns which models' attempts
/// pay off. The row carries no tokens and costs nothing; the gateway's rows
/// already hold the spend. A ledger that cannot be written loses only this
/// lesson, never the run.
fn record_verdict(
    policy: &SusiRunPolicy<'_>,
    task: &str,
    attempt: u32,
    model: &str,
    outcome: Outcome,
) {
    let provider = crate::providers::Registry::builtin()
        .resolve(model)
        .map_or_else(|| "unknown".to_string(), |resolved| resolved.provider);
    let price = policy
        .catalogue
        .and_then(|catalogue| catalogue.price(model))
        .unwrap_or_default();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let entry = LedgerEntry::settle(Dispatch {
        mission: policy.mission.to_string(),
        task: task.to_string(),
        attempt,
        model: model.to_string(),
        provider,
        usage: TokenUsage::default(),
        price,
        started_unix_ms: now_ms,
        ended_unix_ms: now_ms,
        outcome,
    });
    let _ = append_entry(policy.ledger_path, &entry);
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
            ok(""),
            ok(""),
            ok("M docs/harness.md\n"),
            ok(""),
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
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
            task: None,
            precheck: false,
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert_eq!(report.task, "T-AGENT-1");
        assert!(report.committed);
        assert!(report.accepted);
        assert_eq!(report.attempts.len(), 1);
        let prompts = exec.prompts.lock().unwrap();
        assert!(prompts[0].contains("Task id: T-AGENT-1"));
        assert!(prompts[0].contains("Do not run any git command"));
        assert!(prompts[0].contains("before you have read more than ten files"));
        assert!(prompts[0].contains("write docs"));
    }

    #[test]
    fn an_existing_queue_task_is_claimed_not_minted() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".agents").join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("T-EXIST-1.json"),
            r#"{"id":"T-EXIST-1","accept":"cargo test -p susi-harness"}"#,
        )
        .unwrap();
        let target = target(vec![
            ok(&format!("cd {}\n", dir.path().display())),
            ok(""),
            ok(""),
            ok("base123\n"),
            ok(""),
            ok(""),
            ok(""),
            ok("M docs/harness.md\n"),
            ok(""),
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
            worktree_name: "queued".into(),
            title: "existing".into(),
            accept: "cargo test -p susi-harness".into(),
            scopes: vec!["docs/harness.md".into()],
            prompt: "do the queued task".into(),
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
            task: Some("T-EXIST-1".into()),
            precheck: false,
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert_eq!(report.task, "T-EXIST-1");
        assert!(report.committed);
        assert!(report.accepted);
        let calls: Vec<String> = target
            .runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.1.join(" "))
            .collect();
        assert!(calls
            .iter()
            .any(|call| call.starts_with("tasks claim T-EXIST-1")));
        assert!(!calls.iter().any(|call| call.starts_with("tasks add")));
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
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
            task: None,
            precheck: false,
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert_eq!(report.task, "T-AGENT-2");
        assert!(!report.committed);
        assert!(!report.accepted);
        assert_eq!(report.attempts.len(), 1);
    }

    #[test]
    fn a_record_only_commit_is_not_work() {
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join(".agents").join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            tasks.join("T-AGENT-3.json"),
            r#"{"accept":"cargo test -p susi-harness"}"#,
        )
        .unwrap();
        let target = target(vec![
            ok(&format!("cd {}\n", dir.path().display())),
            ok(""),
            ok("added T-AGENT-3 — commit\n"),
            ok(""),
            ok(""),
            ok(""),
            ok("base123\n"),
            ok(""),
            ok(""),
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
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.path().join("state"),
            task: None,
            precheck: false,
        };
        let report = run_task(&target, &spec, &exec, &SusiRunPolicy::unmetered()).unwrap();
        assert!(!report.committed);
        assert!(!report.accepted);
        let calls = target.runner.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|(_, args, _)| args.iter().any(|a| a == "release")),
            "a failed mission must release the claim: {calls:?}"
        );
    }

    #[test]
    fn a_missing_acceptance_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(target(vec![]).acceptance_argv(dir.path(), "T-404").is_err());
    }
    #[test]
    fn commit_formats_before_it_stages() {
        // cargo fmt, git add, git status (non-empty), git commit
        let target = target(vec![ok(""), ok(""), ok("M  src/lib.rs\n"), ok("")]);
        assert!(target.commit_work(Path::new("wt"), "T-AGENT-9").unwrap());
        let calls = target.runner.calls.lock().unwrap();
        let fmt_at = calls
            .iter()
            .position(|(program, args, _)| {
                program == Path::new("cargo")
                    && args.len() == 2
                    && args[0] == "fmt"
                    && args[1] == "--all"
            })
            .expect("cargo fmt --all must run before committing");
        let add_at = calls
            .iter()
            .position(|(program, args, _)| {
                program == Path::new("git") && args.first().map(String::as_str) == Some("add")
            })
            .expect("git add must run");
        assert!(fmt_at < add_at, "formatting must precede staging");
    }
    #[test]
    fn gate_lints_the_packages_the_scopes_touch() {
        let target = target(vec![ok("")]);
        target
            .gate(
                Path::new("wt"),
                &[
                    "crates/susi-gmcp/src/lib.rs".to_string(),
                    "xtask/src/main.rs".to_string(),
                    "Cargo.lock".to_string(),
                ],
            )
            .unwrap();
        let calls = target.runner.calls.lock().unwrap();
        let first = |name: &str| {
            calls.iter().position(|(program, args, _)| {
                program == Path::new("cargo") && args.first().map(String::as_str) == Some(name)
            })
        };
        let clean_at = first("clean").expect("cargo clean must run first");
        let clippy_at = first("clippy").expect("cargo clippy must run");
        assert!(
            clean_at < clippy_at,
            "touched packages must be cleaned before the gate reads them"
        );
        let args = &calls[clippy_at].1;
        assert!(args.contains(&"susi-gmcp".to_string()));
        assert!(args.contains(&"xtask".to_string()));
        assert!(args.contains(&"susi".to_string()));
        assert!(args.contains(&"-D".to_string()));
    }
    #[test]
    fn git_status_returns_the_porcelain_output() {
        let target = target(vec![ok(" M src/lib.rs\n")]);
        let status = target.git_status(Path::new("wt")).unwrap();
        assert_eq!(status, " M src/lib.rs\n");
    }
    #[test]
    fn a_leftover_worktree_is_cleared_and_start_retried_once() {
        let target = target(vec![
            RunOutput {
                status: 1,
                stdout: String::new(),
                stderr: "refusing to reuse an unrelated checkout".to_string(),
            },
            ok(""),
            ok(""),
            ok(""),
            ok(""),
            ok(""),
            ok("cd E:/src/wt\n"),
            ok(""),
        ]);
        let path = target.start_worktree("harness-demo").unwrap();
        assert_eq!(path, PathBuf::from("E:/src/wt"));
        let calls = target.runner.calls.lock().unwrap();
        let args: Vec<String> = calls.iter().map(|(_, a, _)| a.join(" ")).collect();
        assert!(
            args.iter().any(|a| a.contains("worktree remove --force")),
            "the stale worktree must be removed: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.contains("branch -D harness-demo")),
            "the stale branch must be deleted: {args:?}"
        );
    }
    #[test]
    fn a_zero_test_acceptance_is_refused() {
        assert!(SusiTarget::<FakeRunner>::is_test_command(&[
            "cargo".to_string(),
            "test".to_string(),
            "--test".to_string(),
            "x".to_string()
        ]));
        assert!(!SusiTarget::<FakeRunner>::is_test_command(&[
            "scripts/check.sh".to_string()
        ]));
        assert!(!SusiTarget::<FakeRunner>::ran_zero_tests(
            "test result: ok. 3 passed; 0 failed"
        ));
        assert!(SusiTarget::<FakeRunner>::ran_zero_tests(
            "test result: ok. 0 passed; 0 failed"
        ));
        assert!(!SusiTarget::<FakeRunner>::ran_zero_tests("tests pass"));
    }
}
