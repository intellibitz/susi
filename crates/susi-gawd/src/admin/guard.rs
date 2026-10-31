//! Mandates 49/50 (Worktree Workflow, Task Queue) at the git hooks, in Rust:
//! `susi workflow guard <commit|merge|push>` and `susi workflow fetch-refs
//! [remote]`.
//!
//! `.githooks/workflow-guard` carried these rules in bash, and
//! `scripts/fetch-workflow-refs.sh` the one-round-trip fetch the push stage
//! starts with. Git for Windows runs a hook through its own `sh.exe` whatever
//! the host has, so the hook stays a one-line shell stub that execs `susi`;
//! everything it decides is decided here, with `git` run through [`Command`]
//! and no shell, coreutils, jq or flock in between, so an installed `susi.exe`
//! enforces the mandates without Git's MSYS userland on PATH. Linux and macOS
//! behave exactly as the script did; `tests/hook_enforcement.rs` pins every
//! refusal through the real hook and through the binary.
//!
//! ```text
//! commit  — refuse in the primary checkout, and refuse plain commits on main
//! merge   — refuse in the primary checkout (merging into main is allowed)
//!           (a cloud session's clone is its worker's own checkout and is not
//!           refused as "the primary": crate::cloud_session records that)
//! push    — refuse deleting, force-pushing or non-fast-forwarding main;
//!           refuse a non-fast-forward push of any other branch too (a
//!           rewritten feature branch orphans the claim, the CI run and the
//!           review that were taken against the commits it dropped);
//!           refuse a feature branch that is behind origin/main
//!           (sync-before-push); refuse a branch whose commits are not tied to
//!           claimed/closed tasks (scripts/check-workflow-compliance.sh, the
//!           same script CI runs, reading the snapshot fetched here).
//! ```
//!
//! Escape hatches (deliberate, visible, never the default):
//!
//! ```text
//! SUSI_ALLOW_PRIMARY=1  commit in the primary checkout
//! SUSI_ALLOW_MAIN=1     plain commit on main
//! SUSI_ALLOW_FORCE=1    non-fast-forward push of any branch, main included
//! ```
//!
//! The push stage reads what git hands the pre-push hook on stdin: one
//! `<local ref> <local sha> <remote ref> <remote sha>` line per ref.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The null object id git passes for a ref being created or deleted.
const ZERO: &str = "0000000000000000000000000000000000000000";

/// The hook that is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `pre-commit`.
    Commit,
    /// `pre-merge-commit`.
    Merge,
    /// `pre-push`, with the pushed refs on stdin.
    Push,
}

/// The escape hatches, each an environment variable that is set and
/// non-empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `SUSI_ALLOW_PRIMARY`: commit in the primary checkout.
    pub primary: bool,
    /// `SUSI_ALLOW_MAIN`: plain commit on main.
    pub main: bool,
    /// `SUSI_ALLOW_FORCE`: non-fast-forward push of any branch.
    pub force: bool,
}

impl Overrides {
    /// Read from this process's environment.
    #[must_use]
    pub fn from_env() -> Self {
        let set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
        Self {
            primary: set("SUSI_ALLOW_PRIMARY"),
            main: set("SUSI_ALLOW_MAIN"),
            force: set("SUSI_ALLOW_FORCE"),
        }
    }
}

/// A refused stage: what the hook prints on stderr before exiting 1. Empty
/// when the refusing step (the compliance script) already said why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// One pushed ref as the pre-push hook receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedRef<'a> {
    /// The local ref (`refs/heads/x`, or `(delete)`).
    pub local_ref: &'a str,
    /// The local object id ([`ZERO`] for a deletion).
    pub local_sha: &'a str,
    /// The remote ref.
    pub remote_ref: &'a str,
    /// The remote's current object id ([`ZERO`] for a ref it does not have).
    pub remote_sha: &'a str,
}

/// Parse one stdin line as the bash `read -r _lref lsha rref rsha` did: a
/// missing field is empty, extra fields are ignored.
#[must_use]
pub fn pushed_ref(line: &str) -> PushedRef<'_> {
    let mut fields = line.split_whitespace();
    let mut next = || fields.next().unwrap_or("");
    PushedRef {
        local_ref: next(),
        local_sha: next(),
        remote_ref: next(),
        remote_sha: next(),
    }
}

/// Fetch the branch tips and the task refs the push gate judges from `remote`
/// in one network round trip. The caller then reads that exact snapshot
/// (`SUSI_WORKFLOW_REFS_FETCHED=1` tells `scripts/check-workflow-compliance.sh`
/// not to fetch again). `scripts/fetch-workflow-refs.sh` is the same fetch for
/// CI's compliance job, which has no susi binary.
pub fn fetch_workflow_refs(root: &Path, remote: &str) -> Result<(), String> {
    let status = Command::new("git")
        .current_dir(root)
        .args(["fetch", "--quiet", "--prune", remote])
        .arg(format!("+refs/heads/*:refs/remotes/{remote}/*"))
        .arg("+refs/claims/*:refs/claims/*")
        .arg("+refs/closed/*:refs/closed/*")
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("git fetch {remote}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git fetch {remote} failed ({status})"))
    }
}

/// Run one stage against the checkout at `root`. `pushed_refs` is the
/// pre-push hook's stdin (ignored for the other stages).
pub fn run(
    root: &Path,
    stage: Stage,
    pushed_refs: &str,
    overrides: Overrides,
) -> Result<(), Refused> {
    match stage {
        Stage::Commit | Stage::Merge => {
            if primary_checkout(root) && !overrides.primary && !crate::cloud_session::is_cloud(root)
            {
                return Err(Refused(PRIMARY_REFUSED.to_string()));
            }
            if stage == Stage::Commit
                && git(root, &["symbolic-ref", "-q", "--short", "HEAD"]).as_deref() == Some("main")
                && !overrides.main
            {
                return Err(Refused(MAIN_REFUSED.to_string()));
            }
            Ok(())
        }
        Stage::Push => push(root, pushed_refs, overrides),
    }
}

const PRIMARY_REFUSED: &str = "❌ Mandate 49: do not commit in the primary checkout.
   Every agent/user works in its own worktree on its own branch:
       scripts/susi-worktree.sh             # no name needed; prints `cd <path>` — or: susi workflow start
   A cloud session's clone is its own worktree: scripts/cloud-session.sh --mark   (docs/cloud-sessions.md)
   (override, knowingly: SUSI_ALLOW_PRIMARY=1 git commit ...)";

const MAIN_REFUSED: &str = "❌ Mandate 49: no direct commits on main. Commit on your branch, then merge origin/main and push.
   (override, knowingly: SUSI_ALLOW_MAIN=1)";

fn push(root: &Path, pushed_refs: &str, overrides: Overrides) -> Result<(), Refused> {
    let mut fetched = false;
    for line in pushed_refs.lines() {
        let r = pushed_ref(line);
        if r.remote_ref == "refs/heads/main" {
            push_main(root, &r, overrides)?;
        } else if r.remote_ref.starts_with("refs/heads/susi-queue/") {
            // A queue request: a refused compare-and-swap push of the queue's
            // refs, carried as a branch for the relay workflow to replay
            // (scripts/queue-relay.py). Not code; no task, no sync.
            continue;
        } else if r.remote_ref.starts_with("refs/heads/") {
            if !fetched {
                // Never judge code freshness against a cached tracking ref.
                fetch_workflow_refs(root, "origin").map_err(|_| {
                    Refused("Cannot verify origin/main and task refs; push refused.".to_string())
                })?;
                fetched = true;
            }
            push_branch(root, &r, overrides)?;
        }
    }
    Ok(())
}

/// Mandates 49/50 for a feature branch: fast-forward only, synced with
/// origin/main, and every commit tied to a claimed or closed task.
fn push_branch(root: &Path, r: &PushedRef<'_>, overrides: Overrides) -> Result<(), Refused> {
    let branch = r
        .remote_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(r.remote_ref);
    // Mandate 49: published history is never rewritten. Only main used to be
    // checked, so a force-push to a feature branch replaced the commits a task
    // claim, a CI run and a review were taken against, and nothing downstream
    // re-derived any of the three.
    if r.local_sha != ZERO
        && r.remote_sha != ZERO
        && !is_ancestor(root, r.remote_sha, r.local_sha)
        && !overrides.force
    {
        return Err(Refused(format!(
            "❌ Mandate 49: push to {branch} must be a fast-forward (would drop {}).
   A force-push rewrites published history: the task claim, the CI run and the review
   taken against the dropped commits keep pointing at code nobody will ever build.
   Integrate instead of rewriting: git fetch origin && git merge origin/main
   (override, knowingly: SUSI_ALLOW_FORCE=1)",
            short(r.remote_sha)
        )));
    }
    // Mandates 49/50: sync-before-push — a feature branch must contain
    // origin/main before it is pushed (atomic per-task loop).
    if r.local_sha != ZERO && has_ref(root, "origin/main") {
        let behind = git(
            root,
            &[
                "rev-list",
                "--count",
                &format!("{}..origin/main", r.local_sha),
            ],
        )
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
        if behind > 0 {
            return Err(Refused(format!(
                "❌ Mandate 49/50: branch is {behind} commit(s) behind origin/main — sync before push.
   git fetch origin && git merge origin/main
   then push; sync again before the next claim."
            )));
        }
    }
    // Mandates 50/51: every commit on a pushed branch belongs to a task this
    // branch claimed (the same script CI runs server-side; the branch is passed
    // so a claim held elsewhere is refused).
    let compliance = root.join("scripts").join("check-workflow-compliance.sh");
    if r.local_sha != ZERO && executable(&compliance) {
        let base = if has_ref(root, "origin/main") {
            "origin/main".to_string()
        } else {
            git(root, &["rev-list", "--max-parents=0", r.local_sha])
                .and_then(|roots| roots.lines().last().map(str::to_string))
                .unwrap_or_default()
        };
        let local_branch = r
            .local_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(r.local_ref);
        let status = script_command(&compliance)
            .args([&base, r.local_sha, "origin", local_branch])
            .env("SUSI_WORKFLOW_REFS_FETCHED", "1")
            .current_dir(root)
            .stdin(Stdio::null())
            .status();
        if !status.is_ok_and(|s| s.success()) {
            // The script has already said why.
            return Err(Refused(String::new()));
        }
    }
    Ok(())
}

/// Mandate 49 for main: never deleted, never rewound.
fn push_main(root: &Path, r: &PushedRef<'_>, overrides: Overrides) -> Result<(), Refused> {
    if r.local_sha == ZERO {
        return Err(Refused(
            "❌ Mandate 49: deleting main is forbidden.".to_string(),
        ));
    }
    if r.remote_sha == ZERO {
        return Ok(());
    }
    if !is_ancestor(root, r.remote_sha, r.local_sha) && !overrides.force {
        return Err(Refused(format!(
            "❌ Mandate 49: push to main must be a fast-forward (would drop {}).
   git fetch && git merge origin/main, cargo check, push. Never force-push main.",
            short(r.remote_sha)
        )));
    }
    Ok(())
}

/// Whether `root` is the primary checkout: its git dir is the common dir.
fn primary_checkout(root: &Path) -> bool {
    let abs = |what: &str| {
        git(root, &["rev-parse", "--path-format=absolute", what])
            .map(PathBuf::from)
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
    };
    match (abs("--git-dir"), abs("--git-common-dir")) {
        (Some(git_dir), Some(common)) => git_dir == common,
        _ => false,
    }
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    git(root, &["merge-base", "--is-ancestor", ancestor, descendant]).is_some()
}

fn has_ref(root: &Path, name: &str) -> bool {
    git(root, &["rev-parse", "--verify", "--quiet", name]).is_some()
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// `git` in `root`: trimmed stdout on success, `None` otherwise. Hooks run
/// with `GIT_DIR` and friends in the environment, and those are inherited, so
/// the child judges the same worktree the hook was run for.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The bash `-x` test: a regular file (with an execute bit, where there is one).
fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// A command running a repository shell script. Windows cannot exec a script
/// by its shebang, so it goes through Git's POSIX shell with its userland on
/// PATH ([`super::tasks::posix_shell_command`]): the compliance script is the
/// one piece of the push path still written in bash, and it only ever runs
/// under a hook, where Git for Windows has already supplied that shell.
fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = super::tasks::posix_shell_command("bash");
        command.arg(script);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(script)
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn a_pushed_ref_line_is_split_like_read_r() {
        let r = pushed_ref("refs/heads/work abc refs/heads/work 000 extra");
        assert_eq!(r.local_ref, "refs/heads/work");
        assert_eq!(r.local_sha, "abc");
        assert_eq!(r.remote_ref, "refs/heads/work");
        assert_eq!(r.remote_sha, "000");
        let partial = pushed_ref("(delete) 000");
        assert_eq!(partial.local_ref, "(delete)");
        assert_eq!(partial.remote_ref, "");
        assert_eq!(partial.remote_sha, "");
    }

    #[test]
    fn a_line_that_is_not_a_branch_is_ignored_by_the_push_stage() {
        // No fetch, no git: a tag push and a blank line judge nothing.
        let dir = std::env::temp_dir();
        assert_eq!(
            run(
                &dir,
                Stage::Push,
                "refs/tags/v1 abc refs/tags/v1 000\n\n",
                Overrides::default()
            ),
            Ok(())
        );
    }

    #[test]
    fn deleting_main_is_refused_before_any_git_call() {
        let dir = std::env::temp_dir();
        let err = run(
            &dir,
            Stage::Push,
            &format!("(delete) {ZERO} refs/heads/main abc\n"),
            Overrides::default(),
        )
        .err();
        assert!(err.is_some_and(|r| r.0.contains("deleting main is forbidden")));
    }

    #[test]
    fn short_takes_eight_characters_or_everything() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
    }
}
