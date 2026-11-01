//! The susi target adapter.
//!
//! The operations needed to run a task against susi itself: create a worktree
//! with its own worker identity, add and claim a task, run the task's declared
//! acceptance, and integrate through the native local coordinator. Commands run
//! directly without a shell; the command runner is a trait so the adapter is
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

/// `text` (a path git wrote in a `.git` or `gitdir` file) as a path: relative
/// ones are relative to `base`.
fn git_path(base: &Path, text: &str) -> PathBuf {
    let path = PathBuf::from(text.trim());
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// The path a `.git` file at `dot_git` points to (`gitdir: <path>`).
fn gitdir_named_in(dot_git: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let named = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?;
    Some(git_path(dot_git.parent()?, named))
}

/// The two paths name one directory, links and spellings resolved.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The git common directory of the checkout at `repo`: its `.git` directory,
/// or for a linked checkout the one its git dir's `commondir` names.
fn common_dir(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let gitdir = gitdir_named_in(&dot_git)?;
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => Some(git_path(&gitdir, &common)),
        Err(_) => Some(gitdir),
    }
}

/// Whether the worktree's `target/` is a link (a junction on Windows): the
/// clone's old shared target, which an older `workflow start` linked.
fn target_is_link(worktree: &Path) -> bool {
    worktree
        .join("target")
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
}

/// One finished command.
struct LocalClaim {
    name: String,
    base: String,
    scopes: Vec<String>,
}

impl LocalClaim {
    fn require_scope(&self, path: &str) -> EaiResult<()> {
        let path = path.to_ascii_lowercase();
        if self.scopes.iter().any(|scope| {
            path == *scope
                || path
                    .strip_prefix(scope)
                    .is_some_and(|tail| tail.starts_with('/'))
        }) {
            Ok(())
        } else {
            Err(EaiError::process(format!(
                "native claim does not own {path}"
            )))
        }
    }
}

fn local_task_id(name: &str) -> String {
    format!("T-LOCAL-{}-1", name.to_ascii_uppercase())
}

fn validate_task_id(id: &str) -> EaiResult<()> {
    if !id.starts_with("T-")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(EaiError::config(
            "task id must be a namespaced identifier without path separators",
        ));
    }
    Ok(())
}

fn check_task_path(worktree: &Path, path: &Path) -> EaiResult<()> {
    let relative = path
        .strip_prefix(worktree)
        .map_err(|_| EaiError::process("task path escapes the owned worktree"))?;
    let mut current = worktree.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if current
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(EaiError::process(
                "task records and their parents cannot be links",
            ));
        }
    }
    Ok(())
}

fn local_claim(worktree: &Path) -> EaiResult<LocalClaim> {
    let common = common_dir(worktree)
        .ok_or_else(|| EaiError::process("worktree has no Git common directory"))?;
    let directory = common.join("susi-local-workflow/claims");
    let entries = std::fs::read_dir(&directory).map_err(|e| {
        EaiError::process(format!("read native claims {}: {e}", directory.display()))
    })?;
    for entry in entries {
        let path = entry.map_err(|e| EaiError::io(e.to_string()))?.path();
        if path.extension().is_none_or(|ext| ext != "claim") {
            continue;
        }
        if std::fs::symlink_metadata(&path)
            .map_err(|e| EaiError::io(e.to_string()))?
            .file_type()
            .is_symlink()
        {
            return Err(EaiError::process("native claim cannot be a symlink"));
        }
        let data = std::fs::read_to_string(&path).map_err(|e| EaiError::io(e.to_string()))?;
        let lines: Vec<_> = data.lines().collect();
        if lines.len() < 5 || lines[0] != "susi-local-claim-v2" {
            return Err(EaiError::process("invalid native claim record"));
        }
        if !same_dir(Path::new(lines[2]), worktree) {
            continue;
        }
        if !matches!(lines[3].len(), 40 | 64) || !lines[3].bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EaiError::process("native claim has invalid base commit"));
        }
        return Ok(LocalClaim {
            name: lines[1].into(),
            base: lines[3].into(),
            scopes: lines[4..].iter().map(|s| s.to_string()).collect(),
        });
    }
    Err(EaiError::process(
        "no live native coordinator claim owns this worktree",
    ))
}

fn parse_acceptance(text: &str) -> EaiResult<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for ch in text.chars() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
            started = true;
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else if matches!(ch, ';' | '|' | '&' | '<' | '>' | '`') {
            return Err(EaiError::config(
                "acceptance must be a program and arguments, not a shell expression",
            ));
        } else {
            word.push(ch);
            started = true;
        }
    }
    if quote.is_some() {
        return Err(EaiError::config("unterminated acceptance argument quote"));
    }
    if started {
        result.push(word);
    }
    if result.is_empty() || result[0].is_empty() {
        return Err(EaiError::config("acceptance argv cannot be empty"));
    }
    Ok(result)
}

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
        let mut command = Command::new(program);
        if program.file_stem().is_some_and(|name| name == "cargo") {
            if target_is_link(cwd)
                || args
                    .iter()
                    .any(|arg| arg == "--target-dir" || arg.starts_with("--target-dir="))
            {
                return Err(EaiError::process(
                    "agent Cargo commands require the worktree's private target directory",
                ));
            }
            command.env("CARGO_TARGET_DIR", cwd.join("target"));
            command.env("CARGO_NET_OFFLINE", "true");
        }
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
        let status = command
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
    /// only `workflow start`; the target seeding after it, and the agent runs
    /// that follow, run in parallel.
    ///
    /// # Errors
    ///
    /// Returns an error when native start fails or prints no `Worktree:` path.
    pub fn start_worktree(&self, name: &str) -> EaiResult<PathBuf> {
        self.start_scoped_worktree(name, &[], None)
    }

    fn start_scoped_worktree(
        &self,
        name: &str,
        scopes: &[String],
        task: Option<&str>,
    ) -> EaiResult<PathBuf> {
        let id = task
            .map(str::to_owned)
            .unwrap_or_else(|| local_task_id(name));
        validate_task_id(&id)?;
        let mut args = vec![
            "local".into(),
            "workflow".into(),
            "start".into(),
            name.into(),
        ];
        for scope in scopes
            .iter()
            .cloned()
            .chain(std::iter::once(format!(".agents/tasks/{id}.json")))
        {
            args.extend(["--scope".into(), scope]);
        }
        let output = self.susi(args, &self.repo)?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native workflow start refused; existing work is preserved: {}",
                output.tail()
            )));
        }
        parse_worktree_path(&output.stdout).ok_or_else(|| {
            EaiError::process(format!(
                "native workflow start printed no Worktree path: {}",
                output.tail()
            ))
        })
    }

    /// Ask the native coordinator to release a completed claim.
    /// Dirty or unintegrated candidates retain ownership for repair.
    ///
    /// # Errors
    /// Returns an error when the coordinator refuses release.
    pub fn release(&self, worktree: &Path, _id: &str) -> EaiResult<RunOutput> {
        let claim = local_claim(worktree)?;
        let output = self.susi(
            vec![
                "local".into(),
                "workflow".into(),
                "release".into(),
                claim.name,
            ],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native workflow retained the claim and work for repair: {}",
                output.tail()
            )));
        }
        Ok(output)
    }

    /// Verify that an owned worktree still extends its recorded local base.
    /// No network fetch, branch reset, or cleanup is performed.
    ///
    /// # Errors
    /// Returns an error when ownership or ancestry cannot be verified.
    pub fn sync(&self, worktree: &Path) -> EaiResult<()> {
        let claim = local_claim(worktree)?;
        if !self.base_is_ancestor(worktree, &claim.base)? {
            return Err(EaiError::process(
                "native claim base is no longer an ancestor; preserve and repair this worktree",
            ));
        }
        Ok(())
    }

    /// Add a task in the worktree; returns its id.
    ///
    /// # Errors
    ///
    /// Returns an error when the task cannot be added.
    pub fn add_task(&self, worktree: &Path, title: &str, accept: &str) -> EaiResult<String> {
        let claim = local_claim(worktree)?;
        let id = local_task_id(&claim.name);
        let relative = format!(".agents/tasks/{id}.json");
        claim.require_scope(&relative)?;
        let argv = parse_acceptance(accept)?;
        let record = serde_json::json!({"id":id,"title":title,"status":"claimed","owner":claim.name,"scopes":claim.scopes,"accept":{"cmd":argv},"workflow":"susi-local-v2","base":claim.base});
        let path = worktree.join(relative);
        check_task_path(worktree, &path)?;
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| EaiError::io("task has no parent"))?,
        )
        .map_err(|e| EaiError::io(e.to_string()))?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| EaiError::io(format!("preserve existing task {}: {e}", path.display())))?;
        file.write_all(
            serde_json::to_vec_pretty(&record)
                .map_err(|e| EaiError::io(e.to_string()))?
                .as_slice(),
        )
        .map_err(|e| EaiError::io(e.to_string()))?;
        Ok(id)
    }

    /// Claim a task with its reserved scopes.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim is refused (another agent, a stale
    /// branch, overlapping scopes).
    pub fn claim(&self, worktree: &Path, id: &str, scopes: &[String]) -> EaiResult<()> {
        validate_task_id(id)?;
        let claim = local_claim(worktree)?;
        let mut args = vec![
            "local".into(),
            "workflow".into(),
            "extend".into(),
            claim.name.clone(),
        ];
        for scope in scopes
            .iter()
            .cloned()
            .chain(std::iter::once(format!(".agents/tasks/{id}.json")))
        {
            args.extend(["--scope".into(), scope]);
        }
        let output = self.susi(args, worktree)?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native scope reservation refused: {}",
                output.tail()
            )));
        }
        let path = task_file(worktree, id)?;
        if path != worktree.join(format!(".agents/tasks/{id}.json")) {
            return Err(EaiError::process(
                "closed task records cannot be reclaimed or changed",
            ));
        }
        let mut record: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| EaiError::io(e.to_string()))?,
        )
        .map_err(|e| EaiError::config(e.to_string()))?;
        if record.get("status").and_then(Value::as_str) == Some("claimed")
            && record
                .get("owner")
                .and_then(Value::as_str)
                .is_some_and(|owner| owner != claim.name)
        {
            return Err(EaiError::process(
                "task record is already claimed by another owner",
            ));
        }
        record["owner"] = Value::String(claim.name);
        record["status"] = Value::String("claimed".into());
        record["workflow"] = Value::String("susi-local-v2".into());
        record["base"] = Value::String(claim.base);
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&record).map_err(|e| EaiError::io(e.to_string()))?,
        )
        .map_err(|e| EaiError::io(e.to_string()))?;
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
            parse_acceptance(text)?
        } else if let Some(cmd) = accept.get("cmd").and_then(Value::as_array) {
            cmd.iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| EaiError::config("acceptance arguments must all be strings"))
                })
                .collect::<EaiResult<Vec<_>>>()?
        } else {
            return Err(EaiError::config(format!(
                "task {id} has no acceptance argv"
            )));
        };
        if argv.is_empty() || argv[0].is_empty() {
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
        let claim = local_claim(worktree)?;
        claim.require_scope(&format!(".agents/tasks/{id}.json"))?;
        let accepted = self.accept(worktree, id)?;
        let path = task_file(worktree, id)?;
        if path != worktree.join(format!(".agents/tasks/{id}.json")) {
            return Err(EaiError::process(
                "closed task records cannot be reclaimed or changed",
            ));
        }
        let mut record: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| EaiError::io(e.to_string()))?,
        )
        .map_err(|e| EaiError::config(e.to_string()))?;
        record["status"] = Value::String("verified".into());
        record["integration"] = Value::String("pending_native_receipt".into());
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&record).map_err(|e| EaiError::io(e.to_string()))?,
        )
        .map_err(|e| EaiError::io(e.to_string()))?;
        Ok(accepted)
    }

    /// Verify, commit, and integrate work locally before releasing its claim.
    ///
    /// # Errors
    ///
    /// Returns an error when finish stops for repair.
    pub fn finish(&self, worktree: &Path, id: &str) -> EaiResult<RunOutput> {
        let claim = local_claim(worktree)?;
        self.close(worktree, id)?;
        self.commit_work(worktree, id)?;
        let output = self.susi(
            vec![
                "local".into(),
                "workflow".into(),
                "integrate".into(),
                claim.name.clone(),
            ],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native integration failed for {id}; candidate retained: {}",
                output.tail()
            )));
        }
        self.release(worktree, id)?;
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
        let claim = local_claim(worktree)?;
        let fmt = self.runner.run(
            Path::new("cargo"),
            &["fmt".into(), "--all".into()],
            worktree,
        )?;
        if !fmt.success() {
            return Err(EaiError::process(format!("format failed: {}", fmt.tail())));
        }
        let status = self.git(worktree, &["status", "--porcelain"])?;
        if !status.success() {
            return Err(EaiError::process(status.tail()));
        }
        if status.stdout.trim().is_empty() {
            return Ok(false);
        }
        let output = self.susi(
            vec![
                "local".into(),
                "workflow".into(),
                "commit".into(),
                claim.name,
                format!("Complete {id}\n\nTask: {id}"),
            ],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native scoped commit failed: {}",
                output.tail()
            )));
        }
        Ok(true)
    }

    /// Run the repository's compile-and-lint gate for the packages the
    /// task's scopes touch, so a change that would fail `local workflow integrate`
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
        // Every worker owns its target. Refuse old shared links without cleaning
        // or modifying output that may belong to another concurrent agent.
        if target_is_link(worktree) {
            return Err(EaiError::process(
                "refusing a shared target link; create a native isolated worktree",
            ));
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
    /// "commits ahead of `the native claim base`" check is true even when the agent wrote
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
    /// commit ahead of `the native claim base`.
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
                format!("{}..HEAD", local_claim(worktree)?.base),
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
        let claim = local_claim(worktree)?;
        let output = self.susi(
            vec![
                "local".into(),
                "workflow".into(),
                "commit".into(),
                claim.name,
                format!("Record {id}\n\nTask: {id}"),
            ],
            worktree,
        )?;
        if !output.success() {
            return Err(EaiError::process(format!(
                "native task commit failed: {}",
                output.tail()
            )));
        }
        self.head(worktree)
    }

    /// Discard everything a rejected attempt wrote, keeping the task record.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot reset the worktree.
    pub fn reset_to(&self, worktree: &Path, commit: &str) -> EaiResult<()> {
        local_claim(worktree)?;
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EaiError::process(
                "reset requires the exact recorded commit hash",
            ));
        }
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

    /// The paths, other than task records, in which the worktree differs from
    /// `base` — committed on its branch or left in its working tree.
    ///
    /// A mission worktree is reused across runs, and `workflow start` hands
    /// back the one an earlier run left behind, branch and all. The branch
    /// may carry the earlier worker's refused commit (one replaced a
    /// 13,869-line acceptance file with 200 lines), and an acceptance run
    /// there judges that commit, not `the native claim base`. Only `.agents/tasks/`
    /// may differ: the task record is the adapter's own.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot diff the worktree.
    pub fn code_ahead_of(&self, worktree: &Path, base: &str) -> EaiResult<Vec<String>> {
        let diff = self.git(
            worktree,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                "--no-renames",
                base,
                "--",
            ],
        )?;
        if !diff.success() {
            return Err(EaiError::process(format!(
                "git diff --name-only {base} failed: {}",
                diff.tail()
            )));
        }
        Ok(diff
            .stdout
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty() && !path.starts_with(".agents/tasks/"))
            .map(str::to_string)
            .collect())
    }

    /// Refuse prior code changes before reusing an owned native worktree.
    /// Existing edits are preserved for repair, never silently reset.
    ///
    /// # Errors
    /// Returns an error when ownership or an unchanged base cannot be established.
    pub fn reset_reused_worktree(&self, worktree: &Path) -> EaiResult<Vec<String>> {
        let claim = local_claim(worktree)?;
        let ahead = self.code_ahead_of(worktree, &claim.base)?;
        if !ahead.is_empty() {
            return Err(EaiError::process("native worktree contains prior changes; preserve them and repair its claim instead of resetting someone else's work"));
        }
        Ok(ahead)
    }

    /// Why the worktree's changes since `base` delete tests, or `None` when
    /// they keep every one.
    ///
    /// The in-process counterpart of the finish gate's check
    /// (`scripts/parallel-workflow.sh`, `gate_tests_kept`): the `#[test]` and
    /// `#[tokio::test]` attributes of every `.rs` file changed since `base`,
    /// committed or not, are counted at `base` and now, and a total that fell
    /// is a rejection. A test moved between changed files keeps the total; a
    /// deleted one does not. A worker that satisfies an acceptance by
    /// replacing the file that holds the other tests is caught here, before
    /// its work is committed, so the ladder reads a failed attempt and never
    /// a success. `SUSI_GATE_ALLOW_TEST_REMOVAL=1` is the operator's
    /// deliberate override, the same one the gate honours.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot diff the worktree.
    pub fn tests_dropped(&self, worktree: &Path, base: &str) -> EaiResult<Option<String>> {
        let allowed =
            std::env::var_os("SUSI_GATE_ALLOW_TEST_REMOVAL").is_some_and(|value| !value.is_empty());
        self.tests_dropped_unless(worktree, base, allowed)
    }

    fn tests_dropped_unless(
        &self,
        worktree: &Path,
        base: &str,
        allowed: bool,
    ) -> EaiResult<Option<String>> {
        if allowed || base.trim().is_empty() {
            return Ok(None);
        }
        let diff = self.git(
            worktree,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--name-only",
                "--no-renames",
                base,
                "--",
                "*.rs",
            ],
        )?;
        if !diff.success() {
            return Err(EaiError::process(format!(
                "git diff --name-only {base} failed: {}",
                diff.tail()
            )));
        }
        let mut files: Vec<&str> = diff
            .stdout
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect();
        files.sort_unstable();
        files.dedup();
        let mut total_before = 0usize;
        let mut total_after = 0usize;
        let mut dropped = Vec::new();
        for file in files {
            let spec = format!("{base}:{file}");
            let shown = self.git(worktree, &["show", &spec])?;
            // A file that did not exist at the base held no tests.
            let before = if shown.success() {
                count_test_attributes(&shown.stdout)
            } else {
                0
            };
            let after = std::fs::read_to_string(worktree.join(file))
                .map(|text| count_test_attributes(&text))
                .unwrap_or(0);
            total_before += before;
            total_after += after;
            if after < before {
                dropped.push(format!("{file}: {before} test(s) at the base, {after} now"));
            }
        }
        if total_after >= total_before {
            return Ok(None);
        }
        Ok(Some(format!(
            "{total_before} test(s) in the changed files at the base, {total_after} now ({}); a change that makes an acceptance pass by removing other tests is refused",
            dropped.join("; ")
        )))
    }

    /// Why the worktree's changes since `base` are a rewrite rather than an
    /// edit, or `None` when they stay inside `allowance`.
    ///
    /// Over every file changed since `base`, committed or not, the lines
    /// deleted may exceed the lines added by at most `allowance`
    /// (`git diff --numstat`; binary files are not counted). A worker that
    /// replaces a file instead of appending to it deletes the file's whole
    /// body and adds its own few lines, and an acceptance that runs only the
    /// new test never notices. The rejection names the files that lost the
    /// most, with their counts, so the next attempt and the operator see
    /// what was destroyed.
    ///
    /// # Errors
    ///
    /// Returns an error when git cannot diff the worktree.
    pub fn rewrite_beyond(
        &self,
        worktree: &Path,
        base: &str,
        allowance: usize,
    ) -> EaiResult<Option<String>> {
        if base.trim().is_empty() {
            return Ok(None);
        }
        let diff = self.git(
            worktree,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--numstat",
                "--no-renames",
                base,
                "--",
            ],
        )?;
        if !diff.success() {
            return Err(EaiError::process(format!(
                "git diff --numstat {base} failed: {}",
                diff.tail()
            )));
        }
        let mut total_added = 0usize;
        let mut total_deleted = 0usize;
        let mut losses: Vec<(usize, usize, String)> = Vec::new();
        for line in diff.stdout.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(added), Some(deleted), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            // Binary files report `-` for both counts and are not lines.
            let (Ok(added), Ok(deleted)) = (
                added.trim().parse::<usize>(),
                deleted.trim().parse::<usize>(),
            ) else {
                continue;
            };
            total_added += added;
            total_deleted += deleted;
            if deleted > added {
                losses.push((deleted - added, deleted, path.trim().to_string()));
            }
        }
        if total_deleted <= total_added.saturating_add(allowance) {
            return Ok(None);
        }
        losses.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        let named: Vec<String> = losses
            .iter()
            .take(5)
            .map(|(net, deleted, path)| format!("{path}: {deleted} line(s) deleted, {net} net"))
            .collect();
        Ok(Some(format!(
            "{total_deleted} line(s) deleted against {total_added} added since the base, {} net, over the allowance of {allowance} ({}); a worker edits or appends, it does not replace files — a deliberate large rewrite runs with {NET_DELETION_ALLOWANCE_ENV} raised",
            total_deleted - total_added,
            named.join("; ")
        )))
    }

    fn git(&self, worktree: &Path, args: &[&str]) -> EaiResult<RunOutput> {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.runner.run(Path::new("git"), &argv, worktree)
    }
}

/// The number of lines that are a `#[test]` or `#[tokio::test]` attribute,
/// counted the way the finish gate counts them: the attribute alone on its
/// line, or opening an argument list (`#[test(…)]`, `#[tokio::test(flavor =
/// …)]`), so a `#[test_case]` or a `#[tested]` is not one.
#[must_use]
pub fn count_test_attributes(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let Some(rest) = line.trim_start().strip_prefix("#[") else {
                return false;
            };
            let rest = rest.strip_prefix("tokio::").unwrap_or(rest);
            let Some(rest) = rest.strip_prefix("test") else {
                return false;
            };
            matches!(rest.chars().next(), None | Some(']' | '('))
        })
        .count()
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
    /// How many more lines than it adds an attempt may delete, over all the
    /// files it changed since the base, before it is rejected as a rewrite
    /// ([`SusiTarget::rewrite_beyond`]). [`DEFAULT_NET_DELETION_ALLOWANCE`]
    /// unless the operator sets `SUSI_HARNESS_NET_DELETION_ALLOWANCE`
    /// ([`net_deletion_allowance_from_env`]); a legitimate large rewrite is
    /// run with the allowance raised, in the open.
    pub net_deletion_allowance: usize,
}

/// The net lines an attempt may delete before it counts as a rewrite: a worker
/// told to append one test replaced a 13,859-line acceptance file with 95
/// lines, and the acceptance (which ran only the new test) passed. An edit
/// that removes a few hundred lines more than it adds is still an edit; one
/// that removes thousands is the file being replaced.
pub const DEFAULT_NET_DELETION_ALLOWANCE: usize = 200;

/// The environment variable that raises or lowers the allowance for a run.
pub const NET_DELETION_ALLOWANCE_ENV: &str = "SUSI_HARNESS_NET_DELETION_ALLOWANCE";

/// The allowance for this process: [`NET_DELETION_ALLOWANCE_ENV`] when it holds
/// a number, else [`DEFAULT_NET_DELETION_ALLOWANCE`].
#[must_use]
pub fn net_deletion_allowance_from_env() -> usize {
    parse_net_deletion_allowance(std::env::var(NET_DELETION_ALLOWANCE_ENV).ok().as_deref())
}

fn parse_net_deletion_allowance(value: Option<&str>) -> usize {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| text.parse::<usize>().ok())
        .unwrap_or(DEFAULT_NET_DELETION_ALLOWANCE)
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
    /// Whether the accepted attempt left a clean commit ahead of `the native claim base`.
    pub committed: bool,
    /// Whether the declared acceptance passed.
    pub accepted: bool,
    /// Every attempt, in order.
    pub attempts: Vec<SusiAttemptRecord>,
    /// True when the ladder stopped because the budget would be crossed, or
    /// because no model on it can be paid for (every one's last real call was
    /// refused: the key was refused or has no credit). Either way the mission
    /// stops for a top-up rather than fail its remaining tasks unattempted.
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
    let worktree =
        target.start_scoped_worktree(&spec.worktree_name, &spec.scopes, spec.task.as_deref())?;
    // Verify that the reserved worktree has no prior code changes. Refused
    // or abandoned candidates remain intact for their owning agent to repair.
    target.reset_reused_worktree(&worktree)?;
    target.sync(&worktree)?;
    // An existing task already has its record on the branch: claim it and keep
    // the synced head as the base a rejected attempt resets to. A new task is
    // minted here and its record is committed first, for the same reason.
    let (task, base) = match &spec.task {
        Some(existing) => {
            target.claim(&worktree, existing, &spec.scopes)?;
            (
                existing.clone(),
                target.commit_task_record(&worktree, existing)?,
            )
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
    // a model call, and publish the close when it does. "Untouched" is proven,
    // not assumed: the tree must equal `the native claim base` apart from the task
    // record, or the acceptance would judge a reused branch's own commits.
    if spec.precheck
        && !spec.accept.trim().is_empty()
        && target
            .code_ahead_of(&worktree, &local_claim(&worktree)?.base)?
            .is_empty()
        && target.accept(&worktree, &task).is_ok()
        && target.git_status(&worktree)?.trim().is_empty()
    {
        target.close(&worktree, &task)?;
        let closed_committed = target.commit_work(&worktree, &task).unwrap_or(false);
        let finished = target.finish(&worktree, &task);
        if finished.is_err() {
            // The native coordinator retains any unintegrated candidate.
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
    // Only a refusal takes a model off a task's order ([`rank_alternates`]),
    // so an empty order means every model's key was refused or has no
    // credit. The rest of the mission would fail the same way without a
    // call, task after task; the mission stops for a top-up instead, as it
    // does when the budget runs out, and leaves the rest of its tasks for a
    // later run.
    let refused_all = ranked.is_empty();
    let mut ladder = Ladder::new(ranked.clone(), policy.policy).with_contexts(contexts);
    let mut attempts = Vec::new();
    let mut last_outcome = None;
    let mut last_model = spec.model.clone();
    let mut committed = false;
    let mut accepted = false;
    let mut budget_exhausted = false;
    loop {
        let attempt = match ladder.next_attempt() {
            NextAttempt::Attempt(attempt) => attempt,
            NextAttempt::Stop(_) if refused_all => {
                last_outcome = Some(RunOutcome::Failed {
                    message: format!(
                        "no model can run: the last real call of every model on this task's ladder was refused (key refused or no credit); fix the key or top up, and a probe re-admits a model once its refusal is {} minutes old",
                        crate::fleet::PROBE_AFTER_MS / 60_000
                    ),
                });
                budget_exhausted = true;
                break;
            }
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
        let history_rewritten = !target.base_is_ancestor(&worktree, &base)?;
        // Before anything is committed: a worker that made the acceptance
        // pass by deleting the other tests, or by replacing a file instead of
        // editing it, is a failed attempt, whatever it reported, and its tree
        // is not kept for the next model to build on.
        let rejection = if history_rewritten {
            None
        } else if let Some(reason) = target.tests_dropped(&worktree, &base)? {
            Some(format!("the attempt deletes tests: {reason}"))
        } else {
            target
                .rewrite_beyond(&worktree, &base, spec.net_deletion_allowance)?
                .map(|reason| {
                    format!("the attempt rewrites files instead of editing them: {reason}")
                })
        };
        let agent = match (&agent, &rejection) {
            (RunOutcome::Succeeded { .. }, Some(message)) => RunOutcome::Failed {
                message: message.clone(),
            },
            _ => agent,
        };
        let attempt_committed = if matches!(&agent, RunOutcome::Succeeded { .. })
            && !history_rewritten
            && rejection.is_none()
        {
            let committed_now = target.commit_work(&worktree, &task)?;
            committed_now && target.worktree_has_work_after(&worktree, &base)?
        } else {
            false
        };
        // The acceptance command proves the behaviour; the repository's
        // compile-and-lint gate proves the change is publishable. Run it
        // now, while another attempt is still cheap, and hand its output to
        // the next attempt instead of letting `local workflow integrate` reject the
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
        if let Some(message) = &rejection {
            let _ = std::fs::write(attempt_state.join("rejection.txt"), message);
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
        } else if let Some(message) = &rejection {
            // Named here, not classified from the text: a deleted file called
            // `connection.rs` must not read as a transient provider failure.
            (FailureKind::Semantic, message.clone())
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
        // The gateway has just written this attempt's provider failure to the
        // ledger, so the order rebuilt from it no longer lists a provider that
        // refused (key, credit or rate limit). Re-seat the ladder only when
        // that order actually drops the model just tried: otherwise the
        // ladder's own retry accounting (a same-model retry, the attempt cap)
        // must stand, and re-seating would hand it a fresh set of attempts.
        let rearranged = rank_alternates(spec, policy);
        if !rearranged.is_empty() && !rearranged.contains(&attempt.model) {
            ladder.arrange(rearranged);
        }
        // Keep a rejected attempt's work for the next model to finish; only a
        // rewritten history makes the base unusable, and a tree that deleted
        // tests or replaced a file is not handed on either (the next model
        // cannot restore a 13,000-line file, and would be rejected for the
        // same deletion). The next prompt names the acceptance or gate
        // failure to fix.
        if history_rewritten || rejection.is_some() {
            target.reset_to(&worktree, &base)?;
        }
        prompt = format!(
            "{base_prompt}\n\nThe previous attempt was rejected by the repository's quality gate. \
Fix this in the worktree before you finish:\n{message}"
        );
        last_outcome = Some(agent);
    }
    if !accepted {
        // The coordinator retains dirty or unintegrated claims for repair.
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

/// The model order for a task: its own (light) model first, then the strong
/// fixers, then the other light models ([`crate::policy::escalation_order`]),
/// each group in the fleet's order, with models whose last real call was
/// refused left out, and unreliable models, cooling providers and models that
/// just failed transiently moved back ([`crate::policy::arrange`]). The ladder
/// already ranks expiring credit, cache, price and capability; re-sorting it
/// by price here would undo that.
///
/// A refused model is re-admitted here, inside the mission, the way a run
/// re-admits it when it starts: once its refusal is
/// [`crate::fleet::PROBE_AFTER_MS`] old, one probe call decides
/// ([`crate::fleet::readmit`]). Otherwise a key fixed or an account topped up
/// mid-mission stayed off until the next run.
fn rank_alternates(spec: &SusiTaskSpec, policy: &SusiRunPolicy<'_>) -> Vec<String> {
    let now_ms = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    };
    let registry = crate::providers::Registry::builtin();
    let strong = |model: &str| registry.tier_of(model) == Some(crate::providers::Tier::Rescue);
    let ladder = crate::policy::escalation_order(&spec.alternates, &strong);
    let mut entries = crate::ledger::read_entries_reporting(policy.ledger_path);
    if !policy.ledger_path.as_os_str().is_empty() {
        let mut candidates = ladder.clone();
        candidates.push(spec.model.clone());
        let probed = crate::fleet::readmit(
            &entries,
            policy.ledger_path,
            policy.catalogue,
            &candidates,
            now_ms(),
        );
        if !probed.is_empty() {
            entries = crate::ledger::read_entries_reporting(policy.ledger_path);
        }
    }
    order_from(&spec.model, &ladder, &entries, now_ms())
}

/// [`rank_alternates`] on a ledger already read, at `now_unix_ms`.
fn order_from(
    preferred: &str,
    ladder: &[String],
    entries: &[LedgerEntry],
    now_unix_ms: u64,
) -> Vec<String> {
    let registry = crate::providers::Registry::builtin();
    let provider_of = |model: &str| registry.resolve(model).map(|resolved| resolved.provider);
    crate::policy::arrange(
        preferred,
        ladder,
        &crate::policy::Standing {
            history: &crate::policy::history(entries),
            cooling: &crate::policy::cooling_providers(entries, now_unix_ms),
            failed: &crate::policy::in_force(crate::policy::failed_models(entries), now_unix_ms),
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

/// Publish an accepted run through `local workflow integrate`.
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
    validate_task_id(id)?;
    let open = worktree
        .join(".agents")
        .join("tasks")
        .join(format!("{id}.json"));
    if open.is_file() {
        check_task_path(worktree, &open)?;
        return Ok(open);
    }
    let done = worktree
        .join(".agents")
        .join("tasks")
        .join("done")
        .join(format!("{id}.json"));
    if done.is_file() {
        check_task_path(worktree, &done)?;
        return Ok(done);
    }
    Err(EaiError::io(format!(
        "task {id} not found under {}",
        worktree.display()
    )))
}

/// The native coordinator prints the absolute worktree path explicitly.
fn parse_worktree_path(stdout: &str) -> Option<PathBuf> {
    stdout.lines().rev().find_map(|line| {
        line.strip_prefix("Worktree: ")
            .map(|path| PathBuf::from(path.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct NativeState {
        calls: Vec<String>,
        commits: usize,
        changed: bool,
        dirty: bool,
        acceptance_failure: bool,
        zero_tests: bool,
        integration_failure: bool,
        prior_changes: bool,
        dropped_tests: bool,
        rewritten: bool,
        snapshot: Vec<u8>,
    }
    struct NativeRunner {
        directory: PathBuf,
        state: std::sync::Arc<Mutex<NativeState>>,
    }
    fn native_claim_fixture(directory: &Path, name: &str, scopes: &[String]) {
        let claims = directory.join(".git/susi-local-workflow/claims");
        std::fs::create_dir_all(&claims).unwrap();
        std::fs::create_dir_all(directory.join("target")).unwrap();
        std::fs::write(
            claims.join(format!("{name}.claim")),
            format!(
                "susi-local-claim-v2\n{name}\n{}\n{}\n{}\n",
                directory.display(),
                "a".repeat(40),
                scopes.join("\n")
            ),
        )
        .unwrap();
    }
    fn native_target(directory: &Path) -> SusiTarget<NativeRunner> {
        SusiTarget {
            repo: directory.into(),
            susi_program: "susi".into(),
            runner: NativeRunner {
                directory: directory.into(),
                state: Default::default(),
            },
        }
    }
    fn task_snapshot(directory: &Path) -> Vec<u8> {
        let mut paths: Vec<_> = std::fs::read_dir(directory.join(".agents/tasks"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        paths
            .into_iter()
            .flat_map(|p| std::fs::read(p).unwrap_or_default())
            .collect()
    }
    impl CommandRunner for NativeRunner {
        fn run(&self, program: &Path, args: &[String], _cwd: &Path) -> EaiResult<RunOutput> {
            let mut state = self.state.lock().unwrap();
            let call = format!("{} {}", program.display(), args.join(" "));
            state.calls.push(call.clone());
            if args.first().map(String::as_str) == Some("local") {
                match args.get(2).map(String::as_str) {
                    Some("start") => {
                        let scopes: Vec<_> = args[4..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| pair[1].to_ascii_lowercase())
                            .collect();
                        native_claim_fixture(&self.directory, &args[3], &scopes);
                        return Ok(ok(&format!(
                            "Worktree: {}\nTarget: {}\n",
                            self.directory.display(),
                            self.directory.join("target").display()
                        )));
                    }
                    Some("commit") => {
                        state.commits += 1;
                        state.dirty = false;
                        state.snapshot = task_snapshot(&self.directory);
                    }
                    Some("integrate") if state.integration_failure => {
                        return Ok(RunOutput {
                            status: 1,
                            stdout: String::new(),
                            stderr: "candidate verification failed".into(),
                        })
                    }
                    Some("extend" | "integrate" | "release") => {}
                    _ => panic!("unexpected native operation {call}"),
                }
                return Ok(ok("native coordinator verified operation"));
            }
            if program == Path::new("git") {
                if state.changed
                    && state.dropped_tests
                    && args.first().map(String::as_str) == Some("show")
                {
                    return Ok(ok("#[test]\nfn one() {}\n#[test]\nfn two() {}\n"));
                }
                if state.changed && state.rewritten && args.iter().any(|a| a == "--numstat") {
                    return Ok(ok(
                        "95\t13764\ttests/gmcp_requirements.rs\n3\t1\tdocs/harness.md\n",
                    ));
                }
                if args.first().map(String::as_str) == Some("rev-parse") {
                    return Ok(ok(&format!("{:040x}", state.commits + 1)));
                }
                if args.first().map(String::as_str) == Some("status") {
                    return Ok(ok(
                        if state.dirty || state.snapshot != task_snapshot(&self.directory) {
                            " M docs/harness.md\n"
                        } else {
                            ""
                        },
                    ));
                }
                if args.first().map(String::as_str) == Some("log") {
                    return Ok(ok(if state.changed && state.commits > 1 {
                        "commit accepted edit\n"
                    } else {
                        ""
                    }));
                }
                if args.iter().any(|a| a == "diff") && args.iter().any(|a| a == "--name-only") {
                    return Ok(ok(if state.prior_changes {
                        "docs/prior.md\n"
                    } else if state.changed && state.dropped_tests {
                        "tests/gmcp_requirements.rs\n"
                    } else {
                        ""
                    }));
                }
                return Ok(ok(""));
            }
            if program == Path::new("cargo") {
                if args.first().map(String::as_str) == Some("test") {
                    if state.acceptance_failure {
                        return Ok(RunOutput {
                            status: 1,
                            stdout: String::new(),
                            stderr: "actual acceptance failure".into(),
                        });
                    }
                    return Ok(ok(if state.zero_tests {
                        "test result: ok. 0 passed; 0 failed"
                    } else {
                        "test result: ok. 1 passed; 0 failed"
                    }));
                }
                return Ok(ok(""));
            }
            panic!("unexpected external dependency {call}");
        }
    }
    struct NativeExec {
        state: std::sync::Arc<Mutex<NativeState>>,
        outcome: RunOutcome,
        edits: bool,
    }
    impl Runner for NativeExec {
        fn run(&self, _config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            let mut state = self.state.lock().unwrap();
            state.changed = self.edits;
            state.dirty = self.edits;
            Ok(self.outcome.clone())
        }
    }

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
    fn start_worktree_reads_the_native_worktree_field() {
        let target = target(vec![ok("Worktree: E:/src/wt\nTarget: E:/src/wt/target\n")]);
        assert_eq!(
            target.start_worktree("demo").unwrap(),
            PathBuf::from("E:/src/wt")
        );
        let calls = target.runner.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            [
                "local",
                "workflow",
                "start",
                "demo",
                "--scope",
                ".agents/tasks/T-LOCAL-DEMO-1.json"
            ]
        );
    }

    /// Native creation supplies a private target without invoking a shell seeder.
    #[test]
    fn start_worktree_uses_native_private_target_without_shell_seeding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::write(
            dir.path().join("scripts/seed-target.sh"),
            "must not execute",
        )
        .unwrap();
        let target = native_target(dir.path());
        target.start_worktree("demo").unwrap();
        assert!(dir.path().join("target").is_dir());
        assert!(!target_is_link(dir.path()));
        let calls = &target.runner.state.lock().unwrap().calls;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("susi local workflow start demo --scope"));
    }

    #[test]
    fn add_task_returns_the_id_and_claim_reserves_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target
            .start_scoped_worktree("demo", &["docs".into()], None)
            .unwrap();
        let id = target
            .add_task(dir.path(), "proof", "cargo test -p susi-harness")
            .unwrap();
        assert_eq!(id, "T-LOCAL-DEMO-1");
        target.claim(dir.path(), &id, &["docs".into()]).unwrap();
        let record: Value =
            serde_json::from_slice(&std::fs::read(task_file(dir.path(), &id).unwrap()).unwrap())
                .unwrap();
        assert_eq!(record["owner"], "demo");
        assert_eq!(record["status"], "claimed");
        assert_eq!(
            target.acceptance_argv(dir.path(), &id).unwrap(),
            ["cargo", "test", "-p", "susi-harness"]
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

    #[test]
    fn failed_native_integration_retains_candidate_and_claim() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.start_worktree("demo").unwrap();
        let id = target.add_task(dir.path(), "proof", "cargo test").unwrap();
        target.runner.state.lock().unwrap().integration_failure = true;
        assert!(target
            .finish(dir.path(), &id)
            .unwrap_err()
            .to_string()
            .contains("candidate retained"));
        let state = target.runner.state.lock().unwrap();
        assert!(state
            .calls
            .iter()
            .any(|call| call == "susi local workflow integrate demo"));
        assert!(!state
            .calls
            .iter()
            .any(|call| call.contains("workflow release")));
        assert!(local_claim(dir.path()).is_ok());
        let record: Value =
            serde_json::from_slice(&std::fs::read(task_file(dir.path(), &id).unwrap()).unwrap())
                .unwrap();
        assert_eq!(record["integration"], "pending_native_receipt");
    }

    #[test]
    fn task_admission_preserves_existing_records_and_requires_native_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        assert!(target
            .add_task(dir.path(), "no owner", "cargo test")
            .is_err());
        target.start_worktree("demo").unwrap();
        let id = target
            .add_task(dir.path(), "original", "cargo test")
            .unwrap();
        let path = task_file(dir.path(), &id).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(target
            .add_task(dir.path(), "overwrite", "cargo test")
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(task_file(dir.path(), "T-../../escape").is_err());
    }

    #[test]
    fn acceptance_preserves_quoted_arguments_and_rejects_shell_expressions() {
        assert_eq!(
            parse_acceptance(r#"cargo test "a b" --manifest-path "C:\work tree\Cargo.toml""#)
                .unwrap(),
            [
                "cargo",
                "test",
                "a b",
                "--manifest-path",
                r"C:\work tree\Cargo.toml"
            ]
        );
        for value in [
            "cargo test && cargo run",
            "cargo test; next",
            "cargo test | tee",
            "cargo test `echo x`",
            "cargo test \"open",
        ] {
            assert!(parse_acceptance(value).is_err(), "{value}");
        }
    }

    #[test]
    fn linked_task_directory_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.start_worktree("demo").unwrap();
        std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
        link_dir(elsewhere.path(), &dir.path().join(".agents/tasks"));
        assert!(target.add_task(dir.path(), "unsafe", "cargo test").is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
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
        let target = native_target(dir.path());
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            edits: true,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(report.accepted && report.committed);
        assert_eq!(report.task, "T-LOCAL-REUSED-1");
        assert_eq!(report.attempts.len(), 1);
        land(&target, &report).unwrap();
        let calls = &target.runner.state.lock().unwrap().calls;
        assert!(calls
            .iter()
            .any(|s| s == "susi local workflow integrate reused"));
        assert!(calls
            .iter()
            .any(|s| s == "susi local workflow release reused"));
        assert!(!calls.iter().any(|s| s.contains("origin")
            || s.contains(".sh")
            || s.contains("susi tasks")
            || s.contains("susi workflow")));
    }

    #[test]
    fn an_existing_queue_task_is_claimed_not_minted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".agents/tasks")).unwrap();
        std::fs::write(
            dir.path().join(".agents/tasks/T-EXIST-1.json"),
            r#"{"id":"T-EXIST-1","accept":{"cmd":["cargo","test"]}}"#,
        )
        .unwrap();
        let target = native_target(dir.path());
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            edits: true,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), Some("T-EXIST-1"), false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert_eq!(report.task, "T-EXIST-1");
        assert!(report.accepted);
        assert!(!dir
            .path()
            .join(".agents/tasks/T-LOCAL-REUSED-1.json")
            .exists());
        assert!(target
            .runner
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|s| s.contains("Record T-EXIST-1")));
    }

    #[test]
    fn an_agent_failure_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Failed {
                message: "provider failed".into(),
            },
            edits: false,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(!report.accepted);
        assert!(!target
            .runner
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|s| s.contains("workflow integrate")));
    }

    #[test]
    fn a_record_only_commit_is_not_work() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "no edits".into(),
                reported_cost_usd: None,
            },
            edits: false,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(!report.accepted);
        assert_eq!(target.runner.state.lock().unwrap().commits, 1);
    }

    #[test]
    fn a_missing_acceptance_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(target(vec![]).acceptance_argv(dir.path(), "T-404").is_err());
    }
    #[test]
    fn commit_formats_before_it_stages() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.start_worktree("demo").unwrap();
        target.add_task(dir.path(), "record", "cargo test").unwrap();
        assert!(target.commit_work(dir.path(), "T-LOCAL-DEMO-1").unwrap());
        let calls = &target.runner.state.lock().unwrap().calls;
        let fmt = calls.iter().position(|s| s == "cargo fmt --all").unwrap();
        let commit = calls
            .iter()
            .position(|s| s.starts_with("susi local workflow commit demo"))
            .unwrap();
        assert!(fmt < commit);
        assert!(!calls
            .iter()
            .any(|s| s.starts_with("git commit") || s.starts_with("git add")));
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
        // The worktree's target is its own: nothing to clean, and a clean
        // would only recompile what the seed brought.
        assert!(
            first("clean").is_none(),
            "the gate must not clean its own target: {calls:?}"
        );
        let clippy_at = first("clippy").expect("cargo clippy must run");
        assert_eq!(calls.len(), 1, "the gate is one clippy run: {calls:?}");
        let args = &calls[clippy_at].1;
        assert!(args.contains(&"susi-gmcp".to_string()));
        assert!(args.contains(&"xtask".to_string()));
        assert!(args.contains(&"susi".to_string()));
        assert!(args.contains(&"-D".to_string()));
    }
    /// A link at `link` to the directory `to`: a junction on Windows (what
    /// the old linker made, and what any user can create), a symlink elsewhere.
    fn link_dir(to: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
            let out = Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(native(link))
                .arg(native(to))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mklink /J: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(to, link).unwrap();
    }

    /// A shared target is refused without deleting another agent's build output.
    #[test]
    fn gate_refuses_shared_target_without_touching_other_agent_output() {
        let dir = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        std::fs::write(shared.path().join("keep"), "other agent output").unwrap();
        link_dir(shared.path(), &dir.path().join("target"));
        let target = target(Vec::new());
        assert!(target
            .gate(dir.path(), &["crates/susi-core".into()])
            .is_err());
        assert!(target.runner.calls.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(shared.path().join("keep")).unwrap(),
            "other agent output"
        );
    }

    /// Native ownership refusal preserves existing directories and files.
    #[test]
    fn native_start_refusal_preserves_existing_work() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "preserve prior work").unwrap();
        let target = target(vec![RunOutput {
            status: 1,
            stdout: String::new(),
            stderr: "destination already exists; refusing to reuse it".into(),
        }]);
        assert!(target.start_worktree("demo").is_err());
        assert_eq!(target.runner.calls.lock().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
            "preserve prior work"
        );
    }

    #[test]
    fn git_status_returns_the_porcelain_output() {
        let target = target(vec![ok(" M src/lib.rs\n")]);
        let status = target.git_status(Path::new("wt")).unwrap();
        assert_eq!(status, " M src/lib.rs\n");
    }
    #[test]
    fn native_start_refusal_is_not_retried_destructively() {
        let target = target(vec![RunOutput {
            status: 1,
            stdout: String::new(),
            stderr: "agent demo already has a claim".into(),
        }]);
        let error = target.start_worktree("demo").unwrap_err().to_string();
        assert!(error.contains("preserved"));
        assert_eq!(
            target.runner.calls.lock().unwrap().len(),
            1,
            "ownership refusal cannot trigger destructive retry"
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

    fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    /// A gateway row for `model` that ended at `at_ms`.
    fn gateway_row(model: &str, at_ms: u64, outcome: Outcome) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: "m".to_string(),
            task: "T".to_string(),
            attempt: 1,
            model: model.to_string(),
            provider: "deepseek".to_string(),
            usage: TokenUsage::default(),
            price: crate::pricing::ModelPrice::default(),
            started_unix_ms: at_ms,
            ended_unix_ms: at_ms,
            outcome,
        })
    }

    fn one_model_task(dir: &Path) -> SusiTaskSpec {
        SusiTaskSpec {
            worktree_name: "one".into(),
            title: "docs".into(),
            accept: "cargo test -p susi-harness".into(),
            scopes: vec!["docs/harness.md".into()],
            prompt: "write docs".into(),
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: vec!["deepseek-flash".into()],
            timeout: Duration::from_secs(5),
            state_dir: dir.join("state"),
            task: None,
            precheck: false,
            net_deletion_allowance: DEFAULT_NET_DELETION_ALLOWANCE,
        }
    }

    /// The observed failure: a fleet of one model, a 429 that outlived the
    /// same-model retry, and every later task of the mission failed in
    /// seconds with "the attempt cap was reached" and no call made.
    #[test]
    fn a_single_model_whose_last_call_was_rate_limited_still_gets_an_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let now = now_unix_ms();
        append_entry(
            &ledger,
            &gateway_row("deepseek-flash", now - 2, Outcome::Completed),
        )
        .unwrap();
        append_entry(
            &ledger,
            &gateway_row(
                "deepseek-flash",
                now - 1,
                Outcome::ModelFailed {
                    class: crate::ledger::FailureClass::RateLimited,
                },
            ),
        )
        .unwrap();
        let exec = FakeExec {
            outcome: RunOutcome::Failed {
                message: "tests red".into(),
            },
            prompts: Mutex::new(Vec::new()),
        };
        let target = native_target(dir.path());
        let policy = SusiRunPolicy {
            ledger_path: &ledger,
            mission: "m",
            ..SusiRunPolicy::unmetered()
        };
        let report = run_task(&target, &one_model_task(dir.path()), &exec, &policy).unwrap();
        assert_eq!(report.attempts.len(), 1, "the only model must be tried");
        assert_eq!(report.attempts[0].model, "deepseek-flash");
        assert!(!report.budget_exhausted);
        assert_eq!(exec.prompts.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_ladder_whose_every_key_was_refused_stops_the_mission_without_a_call() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        // Refused just now: too recent to probe, so no call leaves the test.
        append_entry(
            &ledger,
            &gateway_row(
                "deepseek-flash",
                now_unix_ms(),
                Outcome::ModelFailed {
                    class: crate::ledger::FailureClass::NoCredit,
                },
            ),
        )
        .unwrap();
        let exec = FakeExec {
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            prompts: Mutex::new(Vec::new()),
        };
        let target = native_target(dir.path());
        let policy = SusiRunPolicy {
            ledger_path: &ledger,
            mission: "m",
            ..SusiRunPolicy::unmetered()
        };
        let report = run_task(&target, &one_model_task(dir.path()), &exec, &policy).unwrap();
        assert!(report.attempts.is_empty());
        assert!(exec.prompts.lock().unwrap().is_empty());
        assert!(
            report.budget_exhausted,
            "the mission stops instead of failing every task"
        );
        let RunOutcome::Failed { message } = &report.agent else {
            panic!("expected a failure, got {:?}", report.agent);
        };
        assert!(message.contains("was refused"), "{message}");
    }

    #[test]
    fn an_order_rebuilt_after_a_refusal_drops_that_provider() {
        // The gateway records a refused provider's failure; the order rebuilt
        // from the ledger must not offer the next attempt to a sibling of the
        // same provider.
        let ladder = vec!["claude-sonnet".to_string(), "deepseek-pro".to_string()];
        let at = 1_000_000;
        let entries = vec![gateway_row(
            "claude-sonnet",
            at,
            Outcome::ModelFailed {
                class: crate::ledger::FailureClass::NoCredit,
            },
        )];
        assert_eq!(
            order_from("claude-sonnet", &ladder, &entries, at + 1),
            vec!["deepseek-pro"],
            "an unfunded provider is not offered again"
        );
    }

    #[test]
    fn a_transient_failure_moves_a_model_back_only_for_the_cooldown() {
        let ladder = vec!["deepseek-flash".to_string(), "deepseek-pro".to_string()];
        let at = 1_000_000;
        let entries = vec![gateway_row(
            "deepseek-flash",
            at,
            Outcome::ModelFailed {
                class: crate::ledger::FailureClass::Timeout,
            },
        )];
        assert_eq!(
            order_from("deepseek-flash", &ladder, &entries, at + 1),
            vec!["deepseek-pro", "deepseek-flash"]
        );
        assert_eq!(
            order_from(
                "deepseek-flash",
                &ladder,
                &entries,
                at + crate::policy::COOLDOWN_MS + 1
            ),
            vec!["deepseek-flash", "deepseek-pro"]
        );
    }

    /// An ancestry check performs no fetch, cleanup, or retry reset.
    #[test]
    fn sync_checks_local_ancestry_without_discarding_edits() {
        let dir = tempfile::tempdir().unwrap();
        native_claim_fixture(dir.path(), "demo", &["docs".into()]);
        let target = target(vec![ok("")]);
        target.sync(dir.path()).unwrap();
        let calls = argv_of(&target);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("git merge-base --is-ancestor "));
        assert!(!calls[0].contains("origin"));
    }

    /// Any other refusal is the caller's to see: retrying would hide it.
    #[test]
    fn sync_reports_a_refusal_that_is_not_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        native_claim_fixture(dir.path(), "demo", &["docs".into()]);
        let target = target(vec![RunOutput {
            status: 1,
            stdout: String::new(),
            stderr: String::new(),
        }]);
        assert!(target
            .sync(dir.path())
            .unwrap_err()
            .to_string()
            .contains("preserve"));
        assert_eq!(target.runner.calls.lock().unwrap().len(), 1);
    }
    fn argv_of(target: &SusiTarget<FakeRunner>) -> Vec<String> {
        target
            .runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(program, args, _)| {
                format!(
                    "{} {}",
                    program
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().to_string()),
                    args.join(" ")
                )
            })
            .collect()
    }

    fn spec_for(dir: &Path, task: Option<&str>, precheck: bool) -> SusiTaskSpec {
        SusiTaskSpec {
            worktree_name: "reused".into(),
            title: "docs".into(),
            accept: "cargo test -p susi-harness".into(),
            scopes: vec!["docs/harness.md".into()],
            prompt: "write docs".into(),
            gateway_url: "http://127.0.0.1:1".into(),
            model: "deepseek-flash".into(),
            alternates: Vec::new(),
            timeout: Duration::from_secs(5),
            state_dir: dir.join("state"),
            task: task.map(str::to_string),
            precheck,
            net_deletion_allowance: DEFAULT_NET_DELETION_ALLOWANCE,
        }
    }

    #[test]
    fn test_attributes_are_counted_the_way_the_gate_counts_them() {
        let text = "#[test]\nfn a() {}\n  #[tokio::test]\n  async fn b() {}\n\
#[tokio::test(flavor = \"multi_thread\")]\nasync fn c() {}\n#[test_case(1)]\nfn d() {}\n\
// a #[test] inside a comment is not the attribute on its own line\n#[cfg(test)]\nmod tests {}\n";
        assert_eq!(count_test_attributes(text), 3);
        assert_eq!(count_test_attributes(""), 0);
    }

    /// The observed failure: `workflow start` reused the worktree an earlier
    /// run left behind, whose branch carried that worker's commit replacing
    /// the acceptance file. The precheck ran there, saw the lone test pass and
    /// closed the task with zero model calls. The branch is put back on
    /// `the native claim base` before the sync, the claim and anything that judges it.
    #[test]
    fn reused_worktree_ahead_with_code_is_preserved_and_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.runner.state.lock().unwrap().prior_changes = true;
        target
            .start_scoped_worktree("demo", &["docs".into()], None)
            .unwrap();
        assert!(target.reset_reused_worktree(dir.path()).is_err());
        assert!(!target
            .runner
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|s| s.contains("reset") || s.contains("clean")));
    }

    /// A record-only branch is left alone: nothing to reset, the sync runs
    /// first as before.
    #[test]
    fn worktree_matching_native_base_is_not_reset() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.start_worktree("demo").unwrap();
        assert!(target.reset_reused_worktree(dir.path()).unwrap().is_empty());
        let calls = &target.runner.state.lock().unwrap().calls;
        assert!(calls.iter().any(|s| s.contains(&"a".repeat(40))));
        assert!(!calls
            .iter()
            .any(|s| s.contains("the native claim base") || s.contains("reset")));
    }

    /// Even with the reset in place, the precheck itself proves the tree it
    /// judges equals the native claim base plus the record; a tree with code ahead is
    /// never closed as "already passes on the untouched base".
    #[test]
    fn precheck_refuses_code_ahead_of_native_base() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.runner.state.lock().unwrap().prior_changes = true;
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "unused".into(),
                reported_cost_usd: None,
            },
            edits: false,
        };
        assert!(run_task(
            &target,
            &spec_for(dir.path(), None, true),
            &exec,
            &SusiRunPolicy::unmetered()
        )
        .is_err());
        assert!(!target
            .runner
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|s| s.contains("integrate")));
    }

    /// The precheck still closes a task whose acceptance passes on a tree that
    /// equals the native claim base plus the record.
    #[test]
    fn the_precheck_closes_a_task_that_passes_on_the_untouched_base() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Failed {
                message: "must not run".into(),
            },
            edits: false,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, true),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(report.accepted);
        assert_eq!(report.model, "precheck");
        assert!(target
            .runner
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|s| s == "susi local workflow integrate reused"));
    }

    /// The worker replaced the acceptance file, keeping its one new test and
    /// dropping the others, and reported success. Before anything is
    /// committed the adapter counts the tests of every changed .rs file
    /// against the base, rejects the attempt as a failure the ladder reads,
    /// and resets the tree so the next model is not handed the deletion.
    #[test]
    fn an_attempt_that_deletes_tests_is_rejected_before_it_is_committed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("tests/gmcp_requirements.rs"),
            "#[test]\nfn one() {}\n",
        )
        .unwrap();
        let target = native_target(dir.path());
        target.runner.state.lock().unwrap().dropped_tests = true;
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            edits: true,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(!report.accepted && !report.committed);
        assert!(report.attempts[0].outcome.contains("deletes tests"));
        let calls = &target.runner.state.lock().unwrap().calls;
        assert!(!calls
            .iter()
            .any(|s| s.starts_with("cargo fmt") || s.contains("workflow integrate")));
        assert!(calls.iter().any(|s| s.starts_with("git reset --hard")));
        assert!(dir
            .path()
            .join("state/T-LOCAL-REUSED-1/attempt-1/rejection.txt")
            .is_file());
    }

    /// A test moved between two changed files keeps the total, so the
    /// attempt is judged on its acceptance like any other.
    #[test]
    fn a_test_moved_between_changed_files_is_not_a_deletion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "#[test]\nfn a() {}\n").unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            "#[test]\nfn b() {}\n#[test]\nfn c() {}\n",
        )
        .unwrap();
        let target = target(vec![
            ok("a.rs\nb.rs\n"),
            ok("#[test]\nfn a() {}\n#[test]\nfn c() {}\n"),
            ok("#[test]\nfn b() {}\n"),
        ]);
        assert_eq!(
            target
                .tests_dropped_unless(dir.path(), "base123", false)
                .unwrap(),
            None
        );
    }

    /// A deleted file's tests count as gone; a file new since the base held
    /// none.
    #[test]
    fn a_deleted_test_file_is_a_deletion_and_a_new_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("new.rs"), "#[test]\nfn n() {}\n").unwrap();
        let target = target(vec![
            ok("gone.rs\nnew.rs\n"),
            ok("#[test]\nfn g1() {}\n#[test]\nfn g2() {}\n"),
            RunOutput {
                status: 128,
                stdout: String::new(),
                stderr: "fatal: path 'new.rs' does not exist in 'base123'".to_string(),
            },
        ]);
        let reason = target
            .tests_dropped_unless(dir.path(), "base123", false)
            .unwrap()
            .expect("two tests became one");
        assert!(
            reason.contains("gone.rs: 2 test(s) at the base, 0 now"),
            "{reason}"
        );
        assert!(!reason.contains("new.rs:"), "{reason}");
    }

    /// The operator's deliberate override, the one the finish gate honours,
    /// lets the deletion through without a diff.
    #[test]
    fn the_test_removal_override_skips_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(vec![]);
        assert_eq!(
            target
                .tests_dropped_unless(dir.path(), "base123", true)
                .unwrap(),
            None
        );
        assert!(target.runner.calls.lock().unwrap().is_empty());
    }
    /// The observed failure: a worker told to append one test replaced the
    /// 13,859-line acceptance file with 95 lines, and the acceptance (only
    /// the new test) passed. The attempt is rejected before it is committed,
    /// the message names the file and the counts, the tree goes back to the
    /// record, and the ladder reads a failure.
    #[test]
    fn an_attempt_that_rewrites_a_file_is_rejected_before_it_is_committed() {
        let dir = tempfile::tempdir().unwrap();
        let target = native_target(dir.path());
        target.runner.state.lock().unwrap().rewritten = true;
        let exec = NativeExec {
            state: target.runner.state.clone(),
            outcome: RunOutcome::Succeeded {
                final_text: "done".into(),
                reported_cost_usd: None,
            },
            edits: true,
        };
        let report = run_task(
            &target,
            &spec_for(dir.path(), None, false),
            &exec,
            &SusiRunPolicy::unmetered(),
        )
        .unwrap();
        assert!(!report.accepted && !report.committed);
        assert!(report.attempts[0].outcome.contains("rewrites files"));
        let calls = &target.runner.state.lock().unwrap().calls;
        assert!(!calls
            .iter()
            .any(|s| s.starts_with("cargo fmt") || s.contains("workflow integrate")));
        assert!(calls.iter().any(|s| s.starts_with("git reset --hard")));
        let saved = std::fs::read_to_string(
            dir.path()
                .join("state/T-LOCAL-REUSED-1/attempt-1/rejection.txt"),
        )
        .unwrap();
        assert!(saved.contains("13764"));
    }

    /// An edit that removes somewhat more than it adds — dead code, a
    /// shortened test — stays within the allowance and is judged on its
    /// acceptance; a binary file's counts are not lines.
    #[test]
    fn a_change_within_the_deletion_allowance_is_not_a_rewrite() {
        let target = target(vec![ok(
            "10\t150\tcrates/x/src/lib.rs\n40\t0\tcrates/x/src/new.rs\n-\t-\tassets/a.png\n",
        )]);
        assert_eq!(
            target
                .rewrite_beyond(Path::new("wt"), "base123", DEFAULT_NET_DELETION_ALLOWANCE)
                .unwrap(),
            None,
            "150 deleted against 50 added is 100 net, under 200"
        );
        let tight = SusiTarget {
            repo: PathBuf::from("repo"),
            susi_program: PathBuf::from("susi"),
            runner: FakeRunner::with(vec![ok("10\t150\tcrates/x/src/lib.rs\n")]),
        };
        let reason = tight
            .rewrite_beyond(Path::new("wt"), "base123", 100)
            .unwrap()
            .expect("140 net over an allowance of 100");
        assert!(
            reason.contains("crates/x/src/lib.rs: 150 line(s) deleted, 140 net"),
            "{reason}"
        );
    }

    /// The allowance is the operator's to raise for a deliberate rewrite;
    /// anything that is not a number is the default.
    #[test]
    fn the_deletion_allowance_is_read_from_the_environment_value() {
        assert_eq!(
            parse_net_deletion_allowance(None),
            DEFAULT_NET_DELETION_ALLOWANCE
        );
        assert_eq!(
            parse_net_deletion_allowance(Some("")),
            DEFAULT_NET_DELETION_ALLOWANCE
        );
        assert_eq!(
            parse_net_deletion_allowance(Some("lots")),
            DEFAULT_NET_DELETION_ALLOWANCE
        );
        assert_eq!(parse_net_deletion_allowance(Some(" 20000 ")), 20_000);
        assert_eq!(parse_net_deletion_allowance(Some("0")), 0);
    }
}
