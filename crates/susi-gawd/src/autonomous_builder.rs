//! The end-to-end autonomous build cycle (Mandates 48–56 in one pass):
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
/// Order matters: the worktree is created only after the claim lands, and
/// the commit happens only after [`tasks::close`] proved the acceptance
/// check — a failed agent leaves the claim to expire, not a half-shipped
/// branch.
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
    // 1. Pick + claim through the shared queue (atomic ref push).
    let open = tasks::list_open(repo);
    let id = pick_task(&open).ok_or_else(|| EaiError::config("no open task to build"))?;
    let task = open
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| EaiError::config(format!("picked task {id} is not open")))?;
    let agent = pick_agent(&task)
        .ok_or_else(|| EaiError::config(format!("no agent qualifies for {id}")))?;
    tasks::claim(
        repo,
        &id,
        builder,
        tasks::DEFAULT_LEASE_HOURS,
        tasks::now_unix(),
    )?;

    // 2. Fresh worktree on its own branch — Mandate 49.
    let branch = format!("agent/{}", id.to_ascii_lowercase());
    let wt = repo.parent().unwrap_or(repo).join(format!("builder-{id}"));
    if let Err(e) = git(
        repo,
        &["worktree", "add", &wt.to_string_lossy(), "-b", &branch],
    ) {
        let _ = git(repo, &["branch", "-D", &branch]);
        return Err(e);
    }

    // 3. Delegate the briefed goal — the contract travels with the task.
    let briefed_goal = brief_task(&wt, &task.goal);
    let delegation = Delegation {
        agent: agent.clone(),
        worktree: wt.clone(),
        briefed_goal,
        task,
    };
    if let Err(e) = delegate(&delegation) {
        let _ = git(
            repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = git(repo, &["branch", "-D", &branch]);
        return Err(e);
    }

    // 4. Verify with the task's own acceptance command (runs it in the
    //    worktree and moves the task file to done/ on success).
    if let Err(e) = tasks::close(&wt, &id, builder) {
        let _ = git(
            repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = git(repo, &["branch", "-D", &branch]);
        return Err(e);
    }

    // 5. Commit the result with the mandatory trailer, then push for
    //    auto-merge to open the PR.
    git(&wt, &["add", "-A"])?;
    let msg = format!("{} ({})\n\nTask: {id}", delegation.task.title, agent);
    git(&wt, &["commit", "-q", "-m", &msg])?;
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
