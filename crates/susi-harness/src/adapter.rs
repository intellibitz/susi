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

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
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
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|error| EaiError::process(format!("run {}: {error}", program.display())))?;
        Ok(RunOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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
    /// # Errors
    ///
    /// Returns an error when `workflow start` fails or prints no `cd <path>`.
    pub fn start_worktree(&self, name: &str) -> EaiResult<PathBuf> {
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
            .and_then(Value::as_str)
            .ok_or_else(|| EaiError::config(format!("task {id} declares no acceptance")))?;
        let argv: Vec<String> = accept.split_whitespace().map(str::to_string).collect();
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
    fn a_missing_acceptance_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(target(vec![]).acceptance_argv(dir.path(), "T-404").is_err());
    }
}
