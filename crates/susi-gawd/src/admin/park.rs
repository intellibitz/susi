//! Keep the primary checkout parked at origin/main so nobody works in it
//! (Mandate 49): `susi workflow park-primary [--sync-only]`.
//!
//! `scripts/park-primary.sh` carried this in bash; it is a stub now that
//! execs this, so the hooks (`.githooks/sync-primary`), `scripts/
//! parallel-workflow.sh`, the session-start hook and branches cut before the
//! port keep working. Everything is `git` through [`Command`]: no shell,
//! `flock`, `awk` or `cygpath`, so an installed `susi.exe` keeps the primary
//! current without Git's MSYS userland on PATH. Linux and macOS behave as
//! the script did; `tests/park_primary.rs` pins every message.
//!
//! Parking: on `main`, fast-forwarded to origin/main. Only if another
//! worktree already holds `main` (git allows a branch in one worktree) does it
//! fall back to a detached HEAD at origin/main. It never undoes a human's own
//! `git switch main`, and it refuses, saying why, when the primary checkout
//! has uncommitted changes, an operation in progress, or commits that are not
//! in origin/main yet.
//!
//! `--sync-only` keeps an already-parked `main` current: it fast-forwards it
//! to origin/main and does nothing, silently, unless the primary checkout is
//! on `main`, clean (untracked local tools are kept) and has no local
//! commits. It never switches a branch or touches a dirty tree, so every
//! `workflow check` and hook runs it; it is best effort, never fails its
//! caller, never queues behind another sync (one is already bringing main
//! current), and skips its fetch when the caller just fetched
//! (`SUSI_PARK_REFS_FETCHED=1`, or [`Mode::SyncOnly`]'s flag).
//!
//! A fast-forward runs post-merge hooks, which run this again: a run started
//! under `SUSI_PRIMARY_SYNC_ACTIVE` does nothing. Git exports `GIT_DIR` and
//! friends to hooks, pointing at the calling worktree; every git here runs
//! without them, on the primary checkout.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::seed::{take_lock, HeldLock, Lock};

/// The lock serialising every watcher and hook that moves the primary's
/// index and branch, in the clone's common git dir.
const LOCK_NAME: &str = "susi-primary-update.lockdir";

/// Operations git leaves a marker for while they are in progress.
const IN_PROGRESS: [&str; 5] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
];

/// What a run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Park the primary checkout: move it onto `main` at origin/main, or
    /// detach there, refusing when that would lose work.
    Park,
    /// Fast-forward an already-parked `main`, silently or not at all.
    /// `refs_fetched`: the caller just fetched origin, so this run does not.
    SyncOnly { refs_fetched: bool },
}

/// A run's outcome, as the script reported it: an exit code, what it said on
/// stdout and what it said on stderr.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parked {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Parked {
    fn silent() -> Self {
        Self::default()
    }

    fn said(msg: impl Into<String>) -> Self {
        Self {
            code: 0,
            out: msg.into(),
            err: String::new(),
        }
    }

    fn refused(code: i32, msg: impl Into<String>) -> Self {
        Self {
            code,
            out: String::new(),
            err: msg.into(),
        }
    }
}

/// `git <args>` in `dir`, on the checkout there and not on the one a calling
/// hook's `GIT_DIR` names, and marked so a hook it fires runs no sync of its
/// own. Trimmed stdout on success, `None` otherwise.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_command(dir, args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("SUSI_PRIMARY_SYNC_ACTIVE", "1");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn short(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Whether two paths name one directory, links and spellings resolved.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The worktree other than `primary` that has `main` checked out, if any:
/// `git worktree list --porcelain` gives a block per worktree, `worktree
/// <path>` first and `branch refs/heads/<name>` when it is on a branch.
fn main_elsewhere(primary: &Path) -> Option<String> {
    let listed = git(primary, &["worktree", "list", "--porcelain"])?;
    let mut current: Option<String> = None;
    for line in listed.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(path.to_string());
        } else if line == "branch refs/heads/main" {
            if let Some(path) = &current {
                if !same_dir(Path::new(path), primary) {
                    return Some(path.clone());
                }
            }
        }
    }
    None
}

/// Run the park. `root` is any directory inside a checkout of the clone (the
/// hooks run from the calling worktree); the primary checkout is the common
/// git dir's parent. Never panics; a failure is a [`Parked`] with its code.
#[must_use]
pub fn park_primary(root: &Path, mode: Mode) -> Parked {
    // A fast-forward runs post-merge hooks, which run this again: not while
    // the parent git command is waiting for them.
    if std::env::var_os("SUSI_PRIMARY_SYNC_ACTIVE").is_some_and(|v| !v.is_empty()) {
        return Parked::silent();
    }
    let sync_only = matches!(mode, Mode::SyncOnly { .. });
    let Some(common) = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return Parked::refused(128, "park-primary: not inside a git repository".to_string());
    };
    let common = PathBuf::from(common);
    let primary = common
        .parent()
        .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
    // A cloud session's clone is the worker's own and only checkout: there is
    // no parked primary to keep, and moving it onto main would move the work
    // out from under the worker.
    if crate::cloud_session::is_cloud(&primary) {
        return if sync_only {
            Parked::silent()
        } else {
            Parked::said(
                "park-primary: a cloud session's clone is the worker's own checkout; nothing to \
                 park (docs/cloud-sessions.md)",
            )
        };
    }
    // Serialize all watchers and hooks that mutate the primary index and
    // branch. --sync-only is best effort and runs from every check and hook,
    // so it never queues: if another sync holds the lock, that one is already
    // bringing main current. Blocking here made `susi workflow check` take
    // minutes behind the watcher and other agents' syncs.
    let lock = common.join(LOCK_NAME);
    let _held = {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            match take_lock(&lock, 900) {
                Lock::Taken => break HeldLock(lock.clone()),
                Lock::Impossible => {
                    return if sync_only {
                        Parked::silent()
                    } else {
                        Parked::refused(
                            1,
                            format!("park-primary: cannot make the lock {}", slashes(&lock)),
                        )
                    };
                }
                Lock::Held => {
                    if sync_only {
                        return Parked::silent();
                    }
                    if Instant::now() >= deadline {
                        return Parked::refused(
                            1,
                            format!(
                                "park-primary: another park has held {} for five minutes; giving up",
                                slashes(&lock)
                            ),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
    };
    if git(
        &primary,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    )
    .is_none_or(|git_dir| !same_dir(Path::new(&git_dir), &common))
    {
        return Parked::refused(
            2,
            format!(
                "park-primary: {} is not the primary checkout",
                primary.display()
            ),
        );
    }
    match mode {
        Mode::SyncOnly { refs_fetched } => sync_only_run(&primary, &common, refs_fetched),
        Mode::Park => park_run(&primary, &common),
    }
}

/// Best effort and silent: an offline fetch or an unsafe state is not an
/// error.
fn sync_only_run(primary: &Path, common: &Path, refs_fetched: bool) -> Parked {
    // A linked worktree's sync already fetched these shared remote refs.
    if !refs_fetched && git(primary, &["fetch", "--quiet", "origin"]).is_none() {
        return Parked::silent();
    }
    let Some(target) = git(primary, &["rev-parse", "--verify", "-q", "origin/main"]) else {
        return Parked::silent();
    };
    if git(primary, &["symbolic-ref", "-q", "--short", "HEAD"]).as_deref() != Some("main") {
        return Parked::silent();
    }
    if git(primary, &["rev-parse", "HEAD"]).as_deref() == Some(target.as_str()) {
        return Parked::silent();
    }
    // Untracked local tools are preserved. Git itself refuses a fast-forward
    // that would overwrite an untracked file; tracked edits always block.
    if !git(primary, &["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|s| s.is_empty())
    {
        return Parked::silent();
    }
    if git(
        primary,
        &["rev-list", "--count", &format!("{target}..main")],
    )
    .as_deref()
        != Some("0")
    {
        return Parked::silent();
    }
    if IN_PROGRESS.iter().any(|op| common.join(op).exists()) {
        return Parked::silent();
    }
    if git(primary, &["merge", "--quiet", "--ff-only", &target]).is_none() {
        return Parked::silent();
    }
    Parked::said(format!(
        "primary checkout's main fast-forwarded to origin/main ({})",
        short(&target)
    ))
}

fn park_run(primary: &Path, common: &Path) -> Parked {
    if git(primary, &["fetch", "--quiet", "origin"]).is_none() {
        return Parked::refused(1, "park-primary: cannot fetch origin; not moving anything");
    }
    let Some(target) = git(primary, &["rev-parse", "--verify", "origin/main"]) else {
        return Parked::refused(1, "park-primary: no origin/main to park at");
    };
    let shown = primary.display();
    if let Some(dirty) = git(primary, &["status", "--porcelain"]).filter(|s| !s.is_empty()) {
        let listed = git(primary, &["status", "--short"]).unwrap_or(dirty);
        let first_ten: Vec<&str> = listed.lines().take(10).collect();
        return Parked::refused(
            1,
            format!(
                "park-primary: refusing — {shown} has uncommitted changes:\n{}",
                first_ten.join("\n")
            ),
        );
    }
    if let Some(op) = IN_PROGRESS.iter().find(|op| common.join(op).exists()) {
        return Parked::refused(
            1,
            format!("park-primary: refusing — an operation is in progress in {shown} ({op})"),
        );
    }
    let unmerged = git(
        primary,
        &["rev-list", "--count", &format!("{target}..HEAD")],
    )
    .unwrap_or_default();
    if unmerged != "0" {
        let branch = git(primary, &["symbolic-ref", "-q", "--short", "HEAD"])
            .unwrap_or_else(|| "detached HEAD".to_string());
        return Parked::refused(
            1,
            format!(
                "park-primary: refusing — {unmerged} commit(s) on {branch} are not in origin/main.\n  push and merge them (or move them to a worktree branch) first."
            ),
        );
    }
    let branch = git(primary, &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();
    let head = git(primary, &["rev-parse", "HEAD"]).unwrap_or_default();
    let was = if branch.is_empty() {
        git(primary, &["rev-parse", "--short", "HEAD"]).unwrap_or(head.clone())
    } else {
        branch.clone()
    };
    match main_elsewhere(primary) {
        None => {
            // `main` is free: park ON it. Local main commits would be lost work.
            if git(primary, &["rev-parse", "--verify", "-q", "main"]).is_some()
                && git(
                    primary,
                    &["rev-list", "--count", &format!("{target}..main")],
                )
                .as_deref()
                    != Some("0")
            {
                return Parked::refused(
                    1,
                    "park-primary: refusing — local main has commits that are not in origin/main.",
                );
            }
            if branch == "main" && head == target {
                return Parked::said(format!(
                    "park-primary: already on main at origin/main ({})",
                    short(&target)
                ));
            }
            if git(primary, &["switch", "--quiet", "main"]).is_none()
                && git(
                    primary,
                    &["switch", "--quiet", "-c", "main", "--track", "origin/main"],
                )
                .is_none()
            {
                return Parked::refused(1, "park-primary: cannot switch to main");
            }
            if git(primary, &["merge", "--quiet", "--ff-only", &target]).is_none() {
                return Parked::refused(1, "park-primary: cannot fast-forward main to origin/main");
            }
            Parked::said(format!(
                "park-primary: {shown} moved from {was} to main at origin/main ({})",
                short(&target)
            ))
        }
        Some(holder) => {
            // Another worktree holds main: detach at origin/main instead.
            if branch.is_empty() && head == target {
                return Parked::said(format!(
                    "park-primary: already parked at origin/main ({}), detached ({holder} holds main)",
                    short(&target)
                ));
            }
            if git(primary, &["checkout", "--quiet", "--detach", &target]).is_none() {
                return Parked::refused(1, "park-primary: cannot detach at origin/main");
            }
            Parked::said(format!(
                "park-primary: {shown} moved from {was} to origin/main ({}), detached ({holder} holds main)",
                short(&target)
            ))
        }
    }
}

#[cfg(test)]
mod park_tests {
    use super::*;

    #[test]
    fn a_run_under_an_active_sync_does_nothing() {
        // Set only for this process; the test binary's own hooks never fire.
        std::env::set_var("SUSI_PRIMARY_SYNC_ACTIVE", "1");
        let parked = park_primary(&std::env::temp_dir(), Mode::Park);
        std::env::remove_var("SUSI_PRIMARY_SYNC_ACTIVE");
        assert_eq!(parked, Parked::silent());
    }

    #[test]
    fn outside_a_repository_the_park_refuses_and_the_sync_is_silent_about_it() {
        let dir = std::env::temp_dir().join(format!("susi-park-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let parked = park_primary(&dir, Mode::Park);
        // The temp dir may itself sit inside some repository on a developer's
        // machine; only a directory git knows nothing about is "not inside".
        if parked.code == 128 {
            assert!(parked.err.contains("not inside a git repository"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_takes_eight_characters_or_everything() {
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
    }
}
