//! The end-to-end autonomous build cycle (Mandates 48–60 in one pass):
//! pick an open task, claim it through the shared queue, delegate the
//! self-build-briefed goal to the chosen agent in a fresh git worktree,
//! verify with the task's own acceptance command via [`admin::tasks`]'s
//! close, commit with the `Task:` trailer and push the branch that
//! auto-merge turns into a PR.
//!
//! Agent selection and the delegation call itself are injected — the
//! cycle is transport-agnostic, so tests drive it with fake agents.

use std::path::{Path, PathBuf};
use std::process::Command;

use susi_core::self_build::brief_task;
use susi_error::{EaiError, EaiResult};

use crate::admin::tasks::{self, Task};

/// Everything the delegated agent needs to see.
#[derive(Debug)]
pub struct Delegation {
    /// The claimed task.
    pub task: Task,
    /// The agent chosen to execute it.
    pub agent: String,
    /// The fresh worktree it owns.
    pub worktree: PathBuf,
    /// The goal with [`susi_core::self_build::BRIEF`] prepended when the
    /// workspace is a SUSI tree — the contract travels with the task.
    pub briefed_goal: String,
}

/// What a completed cycle leaves behind.
#[derive(Debug)]
pub struct BuildOutcome {
    /// Closed task id.
    pub task_id: String,
    /// Agent that executed it.
    pub agent: String,
    /// The worktree the work happened in.
    pub worktree: PathBuf,
    /// Branch pushed for auto-merge to pick up.
    pub branch: String,
    /// Head commit of the pushed branch.
    pub commit: String,
}

fn git(dir: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| EaiError::process(format!("git spawn: {e}")))?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run one full build cycle in `repo` (the primary checkout whose
/// `.agents/tasks/` is the shared queue).
///
/// - `builder`: this agent's queue identity (`INTELLIBITZ`, `SUSI`, …).
/// - `pick_task`: chooses among the open tasks (e.g. highest priority).
/// - `pick_agent`: chooses the executing agent (e.g. the scoreboard's
///   best for the task class); `None` aborts before claiming.
/// - `delegate`: performs the work inside the worktree and returns.
///
/// A fresh synchronized worktree acquires the claim before delegation.
/// Implementation commits precede the acceptance/closure commit; failures
/// preserve work, and ownership remains until remote publication is merged.
///
/// # Errors
/// Propagates queue, git and delegate failures as [`EaiError`].
pub fn run_cycle(
    repo: &Path,
    builder: &str,
    pick_task: impl Fn(&[Task]) -> Option<String>,
    pick_agent: impl Fn(&Task) -> Option<String>,
    delegate: impl Fn(&Delegation) -> EaiResult<()>,
) -> EaiResult<BuildOutcome> {
    // Refuse a cached queue before selecting work.
    tasks::ensure_synced(repo)?;
    // 1. Pick from the synchronized queue.
    let open = tasks::list_open(repo);
    let id = pick_task(&open).ok_or_else(|| EaiError::config("no open task to build"))?;
    let task = open
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| EaiError::config(format!("picked task {id} is not open")))?;
    let agent = pick_agent(&task)
        .ok_or_else(|| EaiError::config(format!("no agent qualifies for {id}")))?;
    // 2. Fresh worktree on its own branch — Mandate 49.
    let branch = format!("agent/{}", id.to_ascii_lowercase());
    let wt = repo.parent().unwrap_or(repo).join(format!("builder-{id}"));
    if let Err(e) = git(
        repo,
        &[
            "worktree",
            "add",
            &wt.to_string_lossy(),
            "-b",
            &branch,
            "origin/main",
        ],
    ) {
        let _ = git(repo, &["branch", "-D", &branch]);
        return Err(e);
    }

    crate::admin::workflow::ensure_infrastructure(&wt);
    tasks::ensure_synced(&wt)?;
    tasks::claim(
        &wt,
        &id,
        builder,
        tasks::DEFAULT_LEASE_HOURS,
        tasks::now_unix(),
    )?;

    // 3. Delegate the briefed goal — the contract travels with the task.
    let briefed_goal = brief_task(&wt, &task.goal);
    let delegation = Delegation {
        agent: agent.clone(),
        worktree: wt.clone(),
        briefed_goal,
        task,
    };
    // Failures preserve the worktree, partial work and lease for diagnosis.
    delegate(&delegation)?;
    // Commit implementation while its task is still open. Closing and code
    // in one commit violates the task trailer's own-tree compliance rule.
    git(&wt, &["add", "-A"])?;
    let msg = format!("{} ({})\n\nTask: {id}", delegation.task.title, agent);
    git(&wt, &["commit", "-q", "--allow-empty", "-m", &msg])?;

    // 4. Verify with the task's own acceptance command (runs it in the
    //    worktree and moves the task file to done/ on success).
    tasks::close(&wt, &id, builder)?;

    // 5. Commit the result with the mandatory trailer, then push for
    //    auto-merge to open the PR.
    git(&wt, &["add", "-A"])?;
    let msg = format!("Close {id} after acceptance\n\nTask: {id}");
    git(&wt, &["commit", "-q", "-m", &msg])?;
    git(&wt, &["fetch", "--quiet", "origin"])?;
    git(&wt, &["merge", "--no-edit", "origin/main"])?;
    // Real SUSI workspaces pass the full branch-push gate, not acceptance alone.
    // Generic task-fixture repositories have no workspace lockfile.
    if wt.join("Cargo.lock").is_file() {
        for args in [
            vec!["fmt", "--all", "--check"],
            vec![
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
            vec!["test", "--workspace", "--locked"],
        ] {
            let status = Command::new("cargo").args(args).current_dir(&wt).status()?;
            if !status.success() {
                return Err(EaiError::process(
                    "autonomous builder verification gate failed; worktree retained",
                ));
            }
        }
    }
    let commit = git(&wt, &["rev-parse", "HEAD"])?;
    git(&wt, &["push", "-q", "origin", &branch])?;

    Ok(BuildOutcome {
        task_id: id,
        agent,
        worktree: wt,
        branch,
        commit,
    })
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use std::fs;

    #[test]
    fn git_helper_runs_version_and_reports_error_on_invalid_subcommand() {
        let ver = git(Path::new("."), &["--version"]).expect("git --version");
        assert!(ver.contains("git version"));

        let err = git(Path::new("."), &["definitely-not-a-git-subcommand-xyz"])
            .expect_err("invalid subcommand must fail");
        assert!(err.to_string().contains("failed"));
    }

    #[test]
    fn run_cycle_rejects_non_git_directory_before_selecting_task() {
        let tmp = std::env::temp_dir().join(format!(
            "susi_autonomous_builder_nongit_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).expect("create temp dir");

        let res = run_cycle(&tmp, "BUILDER", |_| None, |_| None, |_| Ok(()));
        assert!(res.is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_cycle_in_synced_repo_reports_no_open_task_or_unknown_task_id() {
        let root = std::env::temp_dir().join(format!(
            "susi_autonomous_builder_synced_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        fs::create_dir_all(&origin).expect("create origin");
        fs::create_dir_all(&repo).expect("create repo");

        git(&origin, &["init", "--bare", "-q"]).expect("init bare");
        git(&repo, &["init", "-q"]).expect("init repo");
        git(&repo, &["config", "user.email", "test@example.com"]).expect("config email");
        git(&repo, &["config", "user.name", "test"]).expect("config name");
        git(&repo, &["checkout", "-q", "-B", "main"]).expect("checkout main");
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .expect("add remote");
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]).expect("commit init");
        git(&repo, &["push", "-q", "-u", "origin", "main"]).expect("push main");

        let err_none = run_cycle(&repo, "BUILDER", |_| None, |_| None, |_| Ok(()))
            .expect_err("no task picked");
        assert!(err_none.to_string().contains("no open task to build"));

        let err_missing = run_cycle(
            &repo,
            "BUILDER",
            |_| Some("T-MISSING".to_string()),
            |_| None,
            |_| Ok(()),
        )
        .expect_err("unknown task id");
        assert!(err_missing
            .to_string()
            .contains("picked task T-MISSING is not open"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delegation_and_build_outcome_preserve_constructed_fields() {
        let task: Task = serde_json::from_str(
            r#"{
                "id": "T-42",
                "title": "Test builder",
                "goal": "Implement feature",
                "size": "s",
                "deps": [],
                "accept": { "cmd": ["cargo", "test"] },
                "created_by": "TEST",
                "created_unix": 1000
            }"#,
        )
        .expect("deserialize task");
        let wt = PathBuf::from("/tmp/builder-T-42");
        let briefed_goal = brief_task(&wt, &task.goal);
        let delegation = Delegation {
            task,
            agent: "codex".to_string(),
            worktree: wt.clone(),
            briefed_goal,
        };
        assert_eq!(delegation.task.id, "T-42");
        assert_eq!(delegation.agent, "codex");
        assert!(delegation.briefed_goal.contains("Implement feature"));

        let outcome = BuildOutcome {
            task_id: "T-42".to_string(),
            agent: "codex".to_string(),
            worktree: wt,
            branch: "agent/t-42".to_string(),
            commit: "deadbeef".to_string(),
        };
        assert_eq!(outcome.task_id, "T-42");
        assert_eq!(outcome.branch, "agent/t-42");
        assert_eq!(outcome.commit, "deadbeef");
    }
}
