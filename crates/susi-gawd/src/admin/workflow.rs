//! `susi workflow check`: can this agent start work here, right now?
//!
//! The rules (Mandates 49-51) are only followed if an agent learns about them
//! *before* it edits. This gathers facts about the current checkout and turns
//! them into a checklist with the exact fix for each failure:
//!
//! 1. it is the agent's own worktree (not the primary checkout — unless the
//!    clone is a cloud session's, which has no other checkout — not `main`,
//!    not a detached HEAD),
//! 2. it is up to date with `origin/main`,
//! 3. this worktree's git hooks are installed and are its own,
//! 4. the agent holds a live claim on a task,
//! 5. nothing it already accepted is still waiting on `origin/main` (the close
//!    receipt — an agent that reported success it never published cannot slip
//!    into the next task),
//! 6. the tree can actually be synced (no merge in progress, nothing
//!    uncommitted — `sync` and `finish` both refuse a dirty tree, but nothing
//!    used to say so before they did),
//! 7. that claim's lease is not about to lapse,
//! 8. the primary-checkout watcher is alive (it is what keeps the primary
//!    parked at `origin/main`),
//! 9. the release installed on this host is not older than the fixes merged
//!    into `main` (the binary every worker runs only changes at a release).
//!
//! Evaluation is pure ([`evaluate`]) so every combination is unit-tested;
//! [`gather`] is the only part that touches git or the network.
use super::tasks::{self, Claim};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pass,
    /// Could not be determined (offline); does not fail the check.
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub state: State,
    pub detail: String,
    /// The command that fixes a failure; empty when passing.
    pub fix: String,
}

/// Hook facts for one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hooks {
    /// Effective `core.hooksPath` (`None` = git's default `.git/hooks`).
    pub configured: Option<String>,
    /// It resolves to this worktree's own `.githooks`.
    pub points_here: bool,
    /// Required hook files that are missing or not executable.
    pub missing: Vec<String>,
}

/// The release this host installed as `~/.susi/bin/susi`, from the marker
/// `scripts/susi-release-sync.sh` writes beside it.
///
/// Agents drive the whole loop through that binary, and it only changes when a
/// release is cut and promoted — so a fix can be merged, documented as active
/// in `AGENTS.md`, and still absent from the tool every worker runs. A version
/// string cannot reveal this: the version is bumped by the release commit
/// itself and stays identical for every commit merged after it. `0.21.0` was
/// installed 92 commits behind `main`, and released a claim taken on another
/// branch — exactly the protection `f9e62947` had added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseDrift {
    /// The marker exists: a release was installed on this host.
    pub installed: bool,
    /// Commit the installed release was built from.
    pub commit: Option<String>,
    /// Commits `origin/main` has that the installed release never saw;
    /// `None` = could not be counted (offline, or an object we do not have).
    pub behind: Option<u64>,
    /// The marker's commit is not contained in `origin/main` at all.
    pub off_main: bool,
    /// This process *is* that installed binary, not a dev build.
    pub this_process: bool,
}

/// Everything [`evaluate`] needs, gathered once.
#[derive(Debug, Clone)]
pub struct Facts {
    pub root: PathBuf,
    pub primary: bool,
    /// The clone is a cloud session's own (`scripts/cloud-session.sh`): there is
    /// no laptop behind it, so its primary checkout is the worker's place and
    /// there is nothing for a watcher to keep converging.
    pub cloud: bool,
    /// `None` = detached HEAD.
    pub branch: Option<String>,
    /// Commits `origin/main` has that HEAD lacks; `None` = could not fetch.
    pub behind: Option<u64>,
    pub hooks: Hooks,
    pub agent: String,
    /// Claims on the remote, or why they could not be read.
    pub claims: Result<Vec<Claim>, String>,
    /// Accepted tasks of this agent whose close is not on `origin/main` yet, or
    /// why they could not be read. See [`super::tasks::CloseRecord`].
    pub owed: Result<Vec<String>, String>,
    /// `git status --porcelain`: empty = clean, `None` = could not be read.
    pub porcelain: Option<String>,
    /// A merge, rebase, cherry-pick or revert is in progress.
    pub merging: bool,
    /// Seconds since the primary-checkout watcher last wrote its heartbeat.
    pub watcher_age: Option<u64>,
    /// Finished worktrees in this clone this agent could reclaim, and how many
    /// belong to other agents. See [`finished_worktrees`].
    pub stale_mine: u64,
    pub stale_others: u64,
    /// How far the release installed on this host trails `origin/main`.
    pub release: ReleaseDrift,
    pub now: u64,
}

const HOOKS_REQUIRED: [&str; 4] = ["commit-msg", "pre-commit", "pre-push", "workflow-guard"];

/// A commit for a detail line: short when it looks like a sha.
fn short(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn pass(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        state: State::Pass,
        detail: detail.into(),
        fix: String::new(),
    }
}

fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        state: State::Fail,
        detail: detail.into(),
        fix: fix.into(),
    }
}

/// Not blocking, but the agent needs to know: warnings never fail the check.
fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        state: State::Warn,
        detail: detail.into(),
        fix: fix.into(),
    }
}

pub fn evaluate(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();

    // 1. Own worktree. A cloud session's clone has no laptop behind it and no
    //    other checkout: its primary checkout is the worker's own place, so
    //    only `main` and a detached HEAD are refused there.
    out.push(if f.primary && !f.cloud {
        fail(
            "own worktree",
            "this is the primary checkout; nobody works here (Mandate 49)",
            "scripts/susi-worktree.sh <name>   # then cd into the new worktree; a cloud session's clone instead: scripts/cloud-session.sh --mark",
        )
    } else {
        match f.branch.as_deref() {
            None => fail(
                "own worktree",
                "detached HEAD; work happens on a branch of your own",
                "git switch -c <your-branch>",
            ),
            Some("main") => fail(
                "own worktree",
                "on main; main only receives merged branches (Mandate 49)",
                "git switch -c <your-branch>   # or scripts/susi-worktree.sh <name>",
            ),
            Some(b) if f.primary => pass(
                "own worktree",
                format!(
                    "branch {b} in {} (cloud session: this clone is the worker's own)",
                    f.root.display()
                ),
            ),
            Some(b) => pass(
                "own worktree",
                format!("branch {b} in {}", f.root.display()),
            ),
        }
    });

    // 2. Up to date with origin/main.
    out.push(match f.behind {
        Some(0) => pass("up to date", "contains all of origin/main"),
        Some(n) => fail(
            "up to date",
            format!("{n} commit(s) behind origin/main — sync before claim or push (atomic loop)"),
            "git fetch origin && git merge origin/main",
        ),
        None => Check {
            name: "up to date",
            state: State::Fail,
            detail: "could not fetch origin (offline?); freshness unknown".into(),
            fix: "git fetch origin && git merge origin/main".into(),
        },
    });

    // 3. Hooks.
    out.push(if !f.hooks.missing.is_empty() {
        fail(
            "hooks installed",
            format!("missing or not executable: {}", f.hooks.missing.join(", ")),
            "scripts/setup-dev.sh   # merge origin/main first if the hooks are not in this branch",
        )
    } else if !f.hooks.points_here {
        fail(
            "hooks installed",
            format!(
                "core.hooksPath is {} — not this worktree's .githooks",
                f.hooks.configured.as_deref().unwrap_or("unset")
            ),
            "git config --worktree --unset core.hooksPath 2>/dev/null; scripts/setup-dev.sh",
        )
    } else {
        pass(
            "hooks installed",
            format!(
                "core.hooksPath={}",
                f.hooks.configured.as_deref().unwrap_or(".githooks")
            ),
        )
    });

    // 4. Holds a claim.
    out.push(match &f.claims {
        Err(why) => Check {
            name: "holds a claim",
            state: State::Fail,
            detail: format!("claims unavailable: {why}"),
            fix: "susi tasks list".into(),
        },
        Ok(claims) => {
            let mine: Vec<&str> = claims
                .iter()
                .filter(|c| c.agent == f.agent && !c.expired(f.now)
                    && c.branch.as_ref().is_none_or(|branch| Some(branch) == f.branch.as_ref()))
                .map(|c| c.task.as_str())
                .collect();
            if mine.is_empty() {
                fail(
                    "holds a claim",
                    format!("{} holds no live claim; every commit must name a claimed task (Mandate 50)", f.agent),
                    "susi tasks list   # pick one, then: susi tasks claim <id>",
                )
            } else if mine.len() > 1 {
                fail("holds a claim", format!("{} holds multiple claims: {}", f.agent, mine.join(", ")), "susi tasks release <id>   # retain exactly one claim")
            } else {
                pass("holds a claim", format!("{} holds {}", f.agent, mine.join(", ")))
            }
        }
    });

    // 5. Accepted work that is not on origin/main yet. `finish` waits for the
    //    merge, but it waits *inside a process*: Ctrl-C, a crash or a suspend
    //    used to leave an accepted task releasable and the next task claimable,
    //    which looks exactly like an agent reporting success it never
    //    published. The close receipt makes that state visible here, before the
    //    next unit of work starts, and `claim` refuses regardless of what this
    //    checklist says.
    out.push(match &f.owed {
        Err(why) => fail(
            "merge published",
            format!("could not read the accepted-task receipts: {why}"),
            "git fetch origin   # then: susi workflow check",
        ),
        Ok(owed) if owed.is_empty() => pass(
            "merge published",
            "nothing accepted is waiting on origin/main",
        ),
        // Accepted-but-unmerged closes are the pipeline: the agent claims the
        // next task while the merge machinery lands these, so up to
        // `merges_in_flight()` is a warning to watch, not a blocker. Past it
        // is the pileup the receipts exist to refuse — success claimed
        // repeatedly and never published.
        Ok(owed) if owed.len() <= tasks::merges_in_flight() => warn(
            "merge published",
            format!(
                "{} accepted and awaiting merge — up to {} may be in flight while the next task \
                 is claimed",
                owed.join(", "),
                tasks::merges_in_flight()
            ),
            "susi tasks audit   # the branch runs land them",
        ),
        Ok(owed) => {
            let first = owed.first().map(String::as_str).unwrap_or_default();
            fail(
                "merge published",
                format!(
                    "{} were accepted and their closes are not on origin/main; an accepted task \
                     is not done until it is merged, and at most {} may be in flight",
                    owed.join(", "),
                    tasks::merges_in_flight()
                ),
                format!(
                    "susi workflow finish {first}   # or: susi tasks release {first} --abandon <reason>"
                ),
            )
        }
    });

    // 6. Can this tree be synced at all? `sync` refuses a dirty tree and the
    //    gate refuses an unresolved merge, but `check` called both "ready", so
    //    the first symptom was a failed finish. A work in progress is a warning
    //    (agents edit constantly); an unresolved merge is a failure.
    out.push(match (&f.porcelain, f.merging) {
        (_, true) => fail(
            "tree clean",
            "a merge, rebase or cherry-pick is in progress; sync and finish refuse until it is resolved",
            "git status   # finish the resolution, or: git merge --abort",
        ),
        (None, false) => warn(
            "tree clean",
            "could not read `git status`",
            "git status",
        ),
        (Some(status), false) if status.trim().is_empty() => {
            pass("tree clean", "no uncommitted changes")
        }
        (Some(status), false) => warn(
            "tree clean",
            format!(
                "{} uncommitted change(s); sync and the gate refuse a dirty tree",
                status.lines().count()
            ),
            "git status   # commit or stash before sync/finish",
        ),
    });

    // 7. A claim that is close to lapsing: finish renews while it waits, but a
    //    lapse frees the task for another agent, so say so before it happens.
    if let Ok(claims) = &f.claims {
        let mine = claims.iter().find(|c| {
            c.agent == f.agent
                && !c.expired(f.now)
                && c.branch
                    .as_ref()
                    .is_none_or(|branch| Some(branch) == f.branch.as_ref())
        });
        if let Some(mine) = mine {
            let left = mine.lease_until_unix.saturating_sub(f.now);
            out.push(if left < 1800 {
                warn(
                    "lease healthy",
                    format!(
                        "the claim on {} lapses in {}m — after that any agent may take it",
                        mine.task,
                        left / 60
                    ),
                    format!("susi tasks renew {}", mine.task),
                )
            } else {
                pass(
                    "lease healthy",
                    format!("{} holds {} for another {}m", f.agent, mine.task, left / 60),
                )
            });
        }
    }

    // 8. The primary checkout only converges while this clone's watcher runs
    //    (AGENTS.md requires it), and nothing noticed when it died. A cloud
    //    session's clone is disposable: there is nothing to keep converging.
    out.push(if f.cloud {
        pass(
            "primary watcher",
            "not needed: a cloud session's clone is disposable, nothing to keep converging",
        )
    } else {
        match f.watcher_age {
            Some(age) if age <= 120 => pass("primary watcher", format!("heartbeat {age}s ago")),
            Some(age) => warn(
                "primary watcher",
                format!("last heartbeat {age}s ago — the primary checkout is not converging"),
                "susi workflow watch &",
            ),
            None => warn(
                "primary watcher",
                "no heartbeat in this clone — the primary checkout is not converging",
                "susi workflow watch &",
            ),
        }
    });

    // 9. The binary the agent is running. `~/.susi/bin/susi` changes only when
    //    a release is cut and promoted, so a merged fix can be documented as
    //    active while the tool every worker runs still enforces the old rules
    //    (0.21.0 released another agent's claim across branches, which main
    //    refuses). The version cannot show it; only the release's commit can.
    //    Advisory: no agent can fix this alone, and it must not block work.
    out.push(match f.release.commit.as_deref() {
        None if !f.release.installed => pass(
            "installed susi",
            "no release marker in this instance (dev build or fresh host)",
        ),
        None => warn(
            "installed susi",
            "the release marker names no commit — cannot tell which revision agents run",
            "scripts/susi-release-sync.sh",
        ),
        Some(commit) if f.release.off_main => warn(
            "installed susi",
            format!(
                "the installed release was built from {}, which is not on origin/main{}",
                short(commit),
                if f.release.this_process {
                    " (this process)"
                } else {
                    ""
                }
            ),
            "scripts/susi-release-sync.sh   # promotes a release built from main",
        ),
        Some(commit) => match f.release.behind {
            Some(0) => pass(
                "installed susi",
                format!(
                    "release {} is on origin/main{}",
                    short(commit),
                    if f.release.this_process {
                        " (this process)"
                    } else {
                        ""
                    }
                ),
            ),
            Some(n) => warn(
                "installed susi",
                format!(
                    "the installed release {} is {n} commit(s) behind origin/main — workflow fixes merged since are not active for agents that run it",
                    short(commit)
                ),
                "scripts/susi-release-sync.sh   # then re-run: susi workflow check",
            ),
            None => warn(
                "installed susi",
                format!(
                    "release {} is on origin/main, but its distance from it could not be counted (offline?)",
                    short(commit)
                ),
                "git fetch origin && susi workflow check",
            ),
        },
    });

    // 10. Finished worktrees accumulate in a clone — one per task, and nothing
    //    ever reclaimed them (14 here, twelve of them dozens of commits stale).
    //    Advisory: housekeeping must never block the loop, and another agent's
    //    worktree is not this agent's to remove.
    out.push(if f.stale_mine > 0 {
        let mut detail = format!(
            "{} of your finished worktree(s) can be reclaimed",
            f.stale_mine
        );
        if f.stale_others > 0 {
            detail.push_str(&format!(
                " ({} more belong to other agents)",
                f.stale_others
            ));
        }
        warn(
            "stale worktrees",
            detail,
            "scripts/prune-worktrees.sh --apply",
        )
    } else if f.stale_others > 0 {
        pass(
            "stale worktrees",
            format!(
                "{} finished worktree(s) belong to other agents",
                f.stale_others
            ),
        )
    } else {
        pass("stale worktrees", "none to reclaim")
    });

    out
}

/// True when nothing failed (warnings do not block).
pub fn ok(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.state != State::Fail)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(tasks::git_bin())
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    p.is_file()
}

/// Idempotently install the repo's git infrastructure on a checkout that
/// carries `.githooks/`: `core.hooksPath`, the ledger merge driver, and
/// executable bits on the required hooks. Called by the susi binary so a
/// fresh clone converges without anyone running `setup-dev.sh` — any susi
/// invocation inside the repo is the install mechanism.
pub fn ensure_infrastructure(root: &Path) {
    let hooks_dir = root.join(".githooks");
    if !hooks_dir.is_dir() {
        return;
    }
    let existing = git(
        root,
        &[
            "config",
            "--get-regexp",
            "^(core\\.hooksPath|merge\\.ledger\\.driver|gc\\.auto|maintenance\\.auto)$",
        ],
    )
    .unwrap_or_default();
    let has_kv = |key: &str, expected: Option<&str>| {
        existing.lines().any(|line| {
            let mut parts = line.splitn(2, char::is_whitespace);
            let k = parts.next().unwrap_or("");
            let v = parts.next().unwrap_or("").trim();
            k.eq_ignore_ascii_case(key) && expected.is_none_or(|exp| v.eq_ignore_ascii_case(exp))
        })
    };
    if !has_kv("core.hooksPath", Some(".githooks")) {
        // --worktree keeps linked-worktree config local; fall back to the
        // repo config when worktreeConfig isn't enabled (e.g. primary).
        if git(
            root,
            &["config", "--worktree", "core.hooksPath", ".githooks"],
        )
        .is_none()
        {
            git(root, &["config", "core.hooksPath", ".githooks"]);
        }
    }
    if !has_kv("merge.ledger.driver", None) {
        git(
            root,
            &[
                "config",
                "merge.ledger.name",
                "union of appended .agents/evidence.json entries",
            ],
        );
        git(
            root,
            &[
                "config",
                "merge.ledger.driver",
                "sh scripts/merge-ledger.sh %O %A %B",
            ],
        );
    }
    if !has_kv("gc.auto", Some("0")) {
        git(root, &["config", "gc.auto", "0"]);
    }
    if !has_kv("maintenance.auto", Some("false")) {
        git(root, &["config", "maintenance.auto", "false"]);
    }
    #[cfg(unix)]
    for h in HOOKS_REQUIRED {
        let p = hooks_dir.join(h);
        if p.is_file() && !executable(&p) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = p.metadata() {
                let mut perms = m.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&p, perms).ok();
            }
        }
    }
}

/// Gather the facts from `dir` (any path inside the checkout).
pub fn gather(dir: &Path, agent: &str) -> Facts {
    // `SUSI_WORKFLOW_TIMING=1` prints how long each phase took, on stderr:
    // the check runs at the start of every agent's loop, so its cost is paid
    // once per task by every agent in the swarm.
    let timing = std::env::var_os("SUSI_WORKFLOW_TIMING").is_some();
    let started = std::time::Instant::now();
    let mark = |phase: &str| {
        if timing {
            eprintln!("  {:>6} ms  {phase}", started.elapsed().as_millis());
        }
    };
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    let abs =
        |arg: &str| git(&root, &["rev-parse", "--path-format=absolute", arg]).map(PathBuf::from);
    let git_dir = abs("--git-dir");
    let common_dir = abs("--git-common-dir");
    let primary = match (&git_dir, &common_dir) {
        (Some(gd), Some(cd)) => canonical(gd) == canonical(cd),
        _ => false,
    };
    let branch = git(&root, &["symbolic-ref", "-q", "--short", "HEAD"]).filter(|b| !b.is_empty());
    let cloud = crate::cloud_session::is_cloud(&root);
    mark("checkout");

    // Two remote round-trips, and nearly all of their cost is the connection
    // itself (~3 s of SSH setup each). They used to run back to back, and the
    // queue refs were fetched a second time behind the first by the `owed` read
    // below: four fetches, ~14 s, for a check every agent session starts with.
    // They write disjoint refs (`refs/remotes/origin/*` against the claim,
    // close, abandon, merge and verify namespaces), so they overlap; the queue
    // is mirrored once and both views read from it.
    // Finished worktrees pile up: one per task, and nothing reclaims them.
    // Counting them is local and only advisory, so it runs alongside the two
    // network round-trips instead of after them, against the origin/main this
    // clone already has: a worktree that merged a moment ago is counted on
    // the next check.
    let (
        (origin_ok, queue_synced),
        (stale_mine, stale_others),
        (configured, points_here, missing),
        (porcelain, merging),
    ) = std::thread::scope(|scope| {
        let stale = scope.spawn(|| match &common_dir {
            Some(dir) => {
                let primary_dir = Path::new(dir)
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .to_path_buf();
                finished_worktrees(&root, &primary_dir, agent)
            }
            None => (0, 0),
        });
        let hooks = scope.spawn(|| {
            let configured =
                git(&root, &["config", "--get", "core.hooksPath"]).filter(|s| !s.is_empty());
            let hooks_dir = root.join(".githooks");
            let points_here = configured.as_deref().is_some_and(|c| {
                let p = Path::new(c);
                let resolved = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                };
                canonical(&resolved) == canonical(&hooks_dir)
            });
            let missing = HOOKS_REQUIRED
                .iter()
                .filter(|h| !executable(&hooks_dir.join(h)))
                .map(|h| (*h).to_string())
                .collect::<Vec<_>>();
            (configured, points_here, missing)
        });
        let tree = scope.spawn(|| {
            let porcelain = git(&root, &["status", "--porcelain"]);
            let merging = git_dir.as_ref().map_or_else(
                || {
                    ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
                        .iter()
                        .any(|m| git(&root, &["rev-parse", "--verify", "--quiet", m]).is_some())
                        || ["rebase-merge", "rebase-apply"].iter().any(|d| {
                            git(
                                &root,
                                &["rev-parse", "--path-format=absolute", "--git-path", d],
                            )
                            .is_some_and(|p| Path::new(&p).exists())
                        })
                },
                |gd| {
                    ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
                        .iter()
                        .any(|m| gd.join(m).exists())
                        || ["rebase-merge", "rebase-apply"]
                            .iter()
                            .any(|d| gd.join(d).exists())
                },
            );
            (porcelain, merging)
        });
        // When `remote() == "origin"` (the default), `tasks::sync_queue`
        // already mirrors `+refs/heads/*:refs/remotes/origin/*` alongside the
        // queue refs in a single atomic `git fetch`, avoiding a second
        // concurrent SSH fetch that collides on `refs/remotes/origin/main.lock`.
        let default_remote = tasks::is_default_remote();
        let origin_extra = (!default_remote)
            .then(|| scope.spawn(|| git(&root, &["fetch", "--quiet", "origin"]).is_some()));
        let queue = tasks::sync_queue(&root).map_err(|e| e.to_string());
        let origin_ok = match origin_extra {
            Some(handle) => handle.join().unwrap_or(false),
            None => queue.is_ok(),
        };
        (
            (origin_ok, queue),
            stale.join().unwrap_or((0, 0)),
            hooks.join().unwrap_or((None, false, Vec::new())),
            tree.join().unwrap_or((None, false)),
        )
    });
    mark("fetch origin + queue + stale worktrees + hooks + tree");

    // Once `sync_queue` finishes, evaluate all post-fetch reads concurrently.
    let (behind, claims, owed, release) = std::thread::scope(|scope| {
        let behind = scope.spawn(|| {
            origin_ok
                .then(|| git(&root, &["rev-list", "--count", "HEAD..origin/main"]))
                .flatten()
                .and_then(|n| n.parse::<u64>().ok())
        });
        let claims = scope.spawn(|| match &queue_synced {
            Ok(()) => tasks::claims_synced(&root).map_err(|e| e.to_string()),
            Err(e) => Err(e.clone()),
        });
        let owed = scope.spawn(|| match &queue_synced {
            Ok(()) => tasks::unpublished_closes_synced(&root, agent)
                .map(|v| v.into_iter().map(|r| r.task).collect::<Vec<_>>())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.clone()),
        });
        let release = release_drift(&root);
        (
            behind.join().ok().flatten(),
            claims
                .join()
                .unwrap_or_else(|_| Err("claims thread panicked".into())),
            owed.join()
                .unwrap_or_else(|_| Err("owed closes thread panicked".into())),
            release,
        )
    });
    mark("claims + owed closes + release drift");

    // The watcher writes this every loop; a stale stamp is how a dead watcher
    // becomes visible, since the log file stays empty while things go well.
    let now = tasks::now_unix();
    let watcher_age = common_dir
        .as_ref()
        .map(|c| c.join("susi-primary-watch.stamp"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|stamp| now.saturating_sub(stamp));

    Facts {
        root,
        primary,
        cloud,
        branch,
        behind,
        hooks: Hooks {
            configured,
            points_here,
            missing,
        },
        agent: agent.to_string(),
        claims,
        owed,
        porcelain,
        merging,
        watcher_age,
        stale_mine,
        stale_others,
        release,
        now,
    }
}

/// How far the installed release trails `origin/main`, read from the marker its
/// promotion path writes (`~/.susi/bin/susi.build.json`, `scripts/susi-release-sync.sh`).
///
/// `SusiDirs::home_dir()` is `$HOME` (not the instance root) because the
/// promotion script installs to `${HOME}/.susi/bin` regardless of an inherited
/// `SUSI_HOME`; a dev instance is never written there (Mandate 48).
fn release_drift(root: &Path) -> ReleaseDrift {
    let bin = susi_paths::SusiDirs::home_dir().join(".susi/bin");
    let Ok(raw) = std::fs::read_to_string(bin.join("susi.build.json")) else {
        return ReleaseDrift::default();
    };
    let commit = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|doc| {
            doc.get("commit")
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
        .filter(|c| !c.trim().is_empty());
    let Some(commit) = commit else {
        return ReleaseDrift {
            installed: true,
            ..ReleaseDrift::default()
        };
    };
    // `--is-ancestor` succeeds only when the commit is contained in main.
    let off_main = git(
        root,
        &["merge-base", "--is-ancestor", &commit, "origin/main"],
    )
    .is_none();
    // Only commits that changed what the *binary* is built from count. A merge
    // adds no content; a task close moves one record under .agents/tasks/; a
    // test-only or docs commit cannot change what this tool does; and hooks and
    // scripts are read from the checkout, so they are live without a release.
    // Counting any of them made a release read as stale while the tool lacked
    // nothing, and a warning that fires when nothing is wrong is one agents
    // learn to ignore.
    let behind = (!off_main)
        .then(|| {
            git(
                root,
                &[
                    "rev-list",
                    "--no-merges",
                    "--count",
                    &format!("{commit}..origin/main"),
                    "--",
                    "src",
                    "crates",
                    "Cargo.toml",
                    "Cargo.lock",
                    "rust-toolchain.toml",
                    "build.rs",
                ],
            )
        })
        .flatten()
        .and_then(|n| n.parse::<u64>().ok());
    let this_process = std::env::current_exe()
        .ok()
        .map(|exe| canonical(&exe))
        .zip(bin.join("susi").canonicalize().ok())
        .is_some_and(|(running, installed)| running == installed);
    ReleaseDrift {
        installed: true,
        commit: Some(commit),
        behind,
        off_main,
        this_process,
    }
}

/// Counts worktrees in this clone that are *finished* — not the primary, not
/// this one, clean, and already contained in `origin/main`: the same invariants
/// `scripts/prune-worktrees.sh` reclaims on. Returns `(mine, other agents')`.
///
/// A stale worktree is not only disk. It is a directory an agent can be
/// resurrected into, holding a branch that no longer means anything; anything
/// with uncommitted or unmerged work is deliberately not counted.
fn finished_worktrees(root: &Path, primary: &Path, me: &str) -> (u64, u64) {
    let Some(listed) = git(root, &["worktree", "list", "--porcelain"]) else {
        return (0, 0);
    };
    let here = canonical(root);
    let primary = canonical(primary);
    let others_paths = listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|path| {
            let resolved = canonical(path);
            resolved != primary && resolved != here
        })
        .collect::<Vec<_>>();
    // One worktree's answer is a few git calls; a swarm has dozens of
    // worktrees, and visiting them one after another made this housekeeping
    // count the slowest part of `workflow check` (about 9 of its 21 s at 18
    // worktrees). They are independent, so they are asked at once. Within one
    // worktree the cheap questions go first: the full working-tree scan of
    // `git status` runs only for a head already on main.
    let now = tasks::now_unix();
    let active_branches: std::collections::HashSet<String> = tasks::claims_synced(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| !c.expired(now))
        .filter_map(|c| c.branch)
        .collect();
    let finished = |path: &PathBuf| -> Option<bool> {
        let head = git(path, &["rev-parse", "HEAD"])?;
        // `--is-ancestor` succeeds only when the commit is contained.
        git(path, &["merge-base", "--is-ancestor", &head, "origin/main"])?;
        if let Some(branch) = git(path, &["symbolic-ref", "-q", "--short", "HEAD"]) {
            if active_branches.contains(&branch) {
                return None;
            }
        }
        if !git(path, &["status", "--porcelain"]).is_some_and(|s| s.trim().is_empty()) {
            return None;
        }
        let owner = git(path, &["config", "--get", "susi.agent"]).unwrap_or_default();
        Some(!me.is_empty() && owner.eq_ignore_ascii_case(me))
    };
    let answers = std::thread::scope(|scope| {
        let handles = others_paths
            .iter()
            .map(|path| scope.spawn(move || finished(path)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect::<Vec<_>>()
    });
    let mine = answers.iter().filter(|is_mine| **is_mine).count() as u64;
    let others = answers.len() as u64 - mine;
    (mine, others)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeResult {
    pub dest: PathBuf,
    pub branch: String,
    pub agent: String,
    pub reused: bool,
}

/// Give the worktree at `dest` a cargo target of its own, warm from the
/// clone's snapshot of origin/main ([`super::seed`]; `scripts/seed-target.sh`
/// is the stub the shell paths run): the registry crates arrive built, and
/// the workspace crates compile from this worktree's sources. Worktrees used
/// to link `target/` to the primary checkout's, and a crate another worktree
/// had built last was linked into this branch's build. The seeding replaces a
/// link an older susi left (the link only), leaves a target that holds a
/// build alone, honours `SUSI_NO_SEED=1` and never fails. Its report goes to
/// stderr, with this command's: stdout ends in the `cd <path>` a caller
/// reads. In the background: the start never waits for the copy (it held
/// every new worktree for minutes); a build that begins first waits for it
/// (`susi workflow seed-target <dest>`, which the hooks and the finish gate
/// run before their first cargo).
fn seed_target(dest: &Path) {
    super::seed::seed_worktree(Some(dest), true);
}

/// The lock serialising worktree starts within one clone (shared refs and
/// config move), in the common git dir.
const START_LOCK: &str = "susi-worktree-start.lockdir";

/// The script's agent slug: lowercased, every character outside `[a-z0-9]`
/// made a dash (`tr -c 'a-z0-9\n' '-'`), the dashes at the ends dropped.
/// `Codex Agent` names a `codex-agent-…` worktree.
fn agent_slug(agent: &str) -> String {
    agent
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// The script's default worktree name: `<agent>-<yyyymmdd>-<hhmmss>-<pid>`.
fn default_worktree_name(agent: &str) -> String {
    // "YYYY-MM-DD HH:MM:SS" -> "YYYYMMDD-HHMMSS"
    let compact = super::seed::timestamp()
        .replace(':', "")
        .replace(' ', "-")
        .replacen('-', "", 2);
    format!("{agent}-{compact}-{}", std::process::id())
}

/// Native implementation of worktree creation, what `scripts/susi-worktree.sh`
/// did (the script is its stub now): under the clone's start lock, create a
/// linked worktree and branch off `base` (origin/main), configure a unique
/// worker identity, install the hooks and the ledger merge driver, enable the
/// machine's compiler cache where sccache is installed, park the primary
/// checkout, start the primary watcher, and seed the worktree's own cargo
/// target in the background. Every step after the worktree exists is best
/// effort, as it was in the script: the worktree is handed back whatever the
/// park or the watcher said.
pub fn start_worktree(
    root: &Path,
    name: Option<&str>,
    agent_override: Option<&str>,
    base: Option<&str>,
) -> Result<WorktreeResult, String> {
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
    .ok_or_else(|| "cannot locate git-common-dir".to_string())?;
    let primary = common.parent().unwrap_or(root).to_path_buf();

    // A cloud session's clone is the worktree: there is nothing to create, no
    // primary checkout to park and no sibling to share a target with. Make sure
    // it carries the hooks and an identity, and hand the caller back where it
    // already is.
    if crate::cloud_session::is_cloud(root) {
        let branch = git(root, &["symbolic-ref", "-q", "--short", "HEAD"])
            .filter(|b| !b.is_empty() && b.as_str() != "main");
        let Some(branch) = branch else {
            return Err(
                "cloud session: this clone is your worktree, but it is on main or a \
                        detached HEAD — git switch -c <your-branch> first"
                    .to_string(),
            );
        };
        ensure_infrastructure(root);
        use crate::worker_identity::{ensure, Outcome};
        let agent = match ensure(root, None, agent_override) {
            Outcome::Own(token) | Outcome::Explicit(token) => token,
            Outcome::Assigned { token, .. } => token,
            Outcome::Primary | Outcome::Blocked { .. } => String::new(),
        };
        return Ok(WorktreeResult {
            dest: root.to_path_buf(),
            branch,
            agent,
            reused: true,
        });
    }

    // Startup changes shared refs and config; serialize it within this clone.
    // A holder is judged by its pid; a start that died keeps nobody waiting.
    let lock = common.join(START_LOCK);
    let held = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            match super::seed::take_lock(&lock, 900) {
                super::seed::Lock::Taken => break super::seed::HeldLock(lock.clone()),
                super::seed::Lock::Impossible => {
                    return Err(format!("cannot make the start lock {}", lock.display()));
                }
                super::seed::Lock::Held => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "another worktree start has held {} for five minutes; giving up",
                            lock.display()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    };

    let agent = agent_override
        .map(str::to_string)
        .or_else(|| std::env::var("SUSI_AGENT").ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "agent".to_string());
    let mut agent_clean = agent_slug(&agent);
    if agent_clean.is_empty() {
        agent_clean = "agent".to_string();
    }

    let name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => default_worktree_name(&agent_clean),
    };
    let base = base
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .unwrap_or("origin/main");

    if name == "main" || name == "HEAD" || name.is_empty() {
        return Err(format!("refusing branch name '{name}'"));
    }

    let worker: String = name
        .to_ascii_uppercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();

    let _ = git(root, &["fetch", "-q", "origin"]);

    let primary_dir_name = primary
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("susi");
    let dest_name = format!("{primary_dir_name}-{name}");
    let dest = primary.parent().unwrap_or(&primary).join(&dest_name);

    let mut reused = false;
    if dest.is_dir() {
        let dest_common = git(
            &dest,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .map(PathBuf::from);
        let dest_branch = git(&dest, &["symbolic-ref", "-q", "--short", "HEAD"]);
        if dest_common.as_ref() != Some(&common) || dest_branch.as_deref() != Some(&name) {
            return Err("refusing to reuse an unrelated checkout".to_string());
        }
        let owner = git(&dest, &["config", "--worktree", "--get", "susi.agent"]);
        if let Some(ref o) = owner {
            if !o.is_empty() && o != &worker {
                return Err(format!(
                    "{} already belongs to agent {}, not {}.\nOne worktree per worker: pick a different name, or reuse it as {}.",
                    dest.display(), o, worker, o
                ));
            }
        }
        reused = true;
    } else {
        let out = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &name,
                &dest.to_string_lossy(),
                base,
            ])
            .current_dir(root)
            .output()
            .map_err(|e| format!("git worktree add failed: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git worktree add failed: {}", err.trim()));
        }
    }

    let _ = git(root, &["config", "extensions.worktreeConfig", "true"]);
    for key in ["susi.agent", "user.name", "user.email"] {
        let _ = git(&dest, &["config", "--worktree", "--unset-all", key]);
    }
    let _ = git(&dest, &["config", "--worktree", "susi.agent", &worker]);
    let _ = git(&dest, &["config", "--worktree", "user.name", &worker]);
    let email = format!("{}@localhost", worker.to_ascii_lowercase());
    let _ = git(&dest, &["config", "--worktree", "user.email", &email]);

    // What scripts/setup-dev.sh does, once per clone (the config is shared by
    // every worktree of it): the repo's git hooks and the ledger and roadmap
    // merge drivers, so parallel agents converge cleanly. Set whether or not
    // this checkout carries a .githooks directory yet, as the script did.
    for (key, value) in [
        ("core.hooksPath", ".githooks"),
        (
            "merge.ledger.name",
            "union of appended .agents/evidence.json entries",
        ),
        ("merge.ledger.driver", "sh scripts/merge-ledger.sh %O %A %B"),
        (
            "merge.roadmap.name",
            "per-vector merge of .agents/roadmap.json",
        ),
        (
            "merge.roadmap.driver",
            "sh scripts/merge-roadmap.sh %O %A %B",
        ),
        ("gc.auto", "0"),
        ("maintenance.auto", "false"),
    ] {
        let _ = git(&dest, &["config", key, value]);
    }
    eprintln!("hooks: .githooks; merge drivers: ledger, roadmap");
    ensure_infrastructure(&dest);

    // The machine's compiler cache: what the seeded snapshot cannot hold
    // compiles once on this machine, not once per worktree. Only where
    // sccache is installed; nothing is installed here, and SUSI_NO_SCCACHE=1
    // leaves the user's cargo config alone. The setup is a bash script still
    // (not one this binary runs for the loop), so it goes through Git's shell.
    if std::env::var("SUSI_NO_SCCACHE").ok().as_deref() != Some("1")
        && dest.join("scripts").join("setup-sccache.sh").is_file()
        && on_path("sccache")
    {
        let mut setup = tasks::posix_shell_command("bash");
        let _ = setup
            .arg(dest.join("scripts").join("setup-sccache.sh"))
            .current_dir(&dest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();
    }

    // Keep the primary checkout parked at origin/main so it never drifts onto
    // a stale branch that invites work (non-fatal: it refuses if it has real
    // work, and says so).
    let parked = super::park::park_primary(root, super::park::Mode::Park);
    if !parked.out.is_empty() {
        eprintln!("{}", parked.out);
    }
    if !parked.err.is_empty() {
        eprintln!("{}", parked.err);
    }
    if parked.code != 0 {
        eprintln!("note: primary checkout not parked (see above); it is still not for work");
    }

    // A remote merge cannot invoke local hooks: keep a local singleton
    // watcher (scripts/parallel-workflow.sh start-watch, bash until that
    // script is ported). SUSI_PRIMARY_WATCH_DISABLE=1 leaves it to the host.
    let watcher = dest.join("scripts").join("parallel-workflow.sh");
    if watcher.is_file() && std::env::var_os("SUSI_PRIMARY_WATCH_DISABLE").is_none() {
        let mut start_watch = tasks::posix_shell_command("bash");
        let _ = start_watch
            .arg(&watcher)
            .arg("start-watch")
            .current_dir(&dest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();
    }

    // Last, as the script did it: the copy takes about a minute, and until
    // the identity above is written the worktree has no owner, so a
    // concurrent `workflow start` with the same name would take the reuse
    // path and claim this directory too. The start lock is let go first: the
    // copy runs in the background and holds nothing of this start's.
    drop(held);
    seed_target(&dest);

    Ok(WorktreeResult {
        dest,
        branch: name,
        agent: worker,
        reused,
    })
}

/// Whether `program` is on this process's PATH.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|dir| dir.join(program).is_file() || dir.join(format!("{program}.exe")).is_file())
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrunedItem {
    pub path: PathBuf,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub items: Vec<PrunedItem>,
    pub kept: u64,
    pub reclaimed: u64,
    pub others: u64,
    pub failed: u64,
}

/// Native implementation of worktree pruning:
/// reclaims finished agent worktrees that have merged into origin/main.
pub fn prune_worktrees(
    root: &Path,
    apply: bool,
    all_agents: bool,
    me: &str,
) -> Result<PruneReport, String> {
    if tasks::sync_queue(root).is_err()
        || (!tasks::is_default_remote() && git(root, &["fetch", "--quiet", "origin"]).is_none())
    {
        return Err(
            "cannot reach origin: refusing to guess which worktrees are finished".to_string(),
        );
    }

    let now = tasks::now_unix();
    let active_claims: std::collections::HashMap<String, String> = tasks::claims_synced(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| !c.expired(now))
        .filter_map(|c| Some((c.branch?, c.task)))
        .collect();

    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(PathBuf::from)
    .ok_or_else(|| "cannot locate git-common-dir".to_string())?;
    let primary = canonical(common.parent().unwrap_or(root));
    let here = canonical(root);

    let listed = git(root, &["worktree", "list", "--porcelain"])
        .ok_or_else(|| "git worktree list failed".to_string())?;

    let wt_paths: Vec<PathBuf> = listed
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect();

    enum Candidate {
        Done(PrunedItem, &'static str),
        Reclaimable(PathBuf, PathBuf),
    }

    let inspect = |wt: &PathBuf| -> Candidate {
        let path = canonical(wt);
        if path == primary {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "primary",
                    detail: path.display().to_string(),
                },
                "none",
            );
        }
        if path == here {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "current",
                    detail: path.display().to_string(),
                },
                "none",
            );
        }

        let Some(head) = git(wt, &["rev-parse", "HEAD"]) else {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "unreadable",
                    detail: path.display().to_string(),
                },
                "kept",
            );
        };

        if let Some(branch) = git(wt, &["symbolic-ref", "-q", "--short", "HEAD"]) {
            if let Some(task) = active_claims.get(&branch) {
                return Candidate::Done(
                    PrunedItem {
                        path: path.clone(),
                        status: "claimed",
                        detail: format!("active claim ({task}): {}", path.display()),
                    },
                    "kept",
                );
            }
        }

        if !git(wt, &["status", "--porcelain"]).is_some_and(|s| s.trim().is_empty()) {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "uncommitted",
                    detail: format!("uncommitted changes: {}", path.display()),
                },
                "kept",
            );
        }

        if git(wt, &["merge-base", "--is-ancestor", &head, "origin/main"]).is_none() {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "unmerged",
                    detail: format!("unmerged commits: {}", path.display()),
                },
                "kept",
            );
        }

        let owner = git(wt, &["config", "--get", "susi.agent"]).unwrap_or_default();
        let mine = !me.is_empty() && owner.eq_ignore_ascii_case(me);
        if !mine && !all_agents {
            return Candidate::Done(
                PrunedItem {
                    path: path.clone(),
                    status: "other_agent",
                    detail: format!(
                        "another agent ({}): {}",
                        if owner.is_empty() { "no owner" } else { &owner },
                        path.display()
                    ),
                },
                "others",
            );
        }

        Candidate::Reclaimable(wt.clone(), path)
    };

    let inspected = std::thread::scope(|scope| {
        let handles: Vec<_> = wt_paths
            .iter()
            .map(|wt| scope.spawn(move || inspect(wt)))
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect::<Vec<_>>()
    });

    let mut report = PruneReport {
        items: Vec::new(),
        kept: 0,
        reclaimed: 0,
        others: 0,
        failed: 0,
    };

    for candidate in inspected {
        match candidate {
            Candidate::Done(item, bucket) => {
                match bucket {
                    "kept" => report.kept += 1,
                    "others" => report.others += 1,
                    _ => {}
                }
                report.items.push(item);
            }
            Candidate::Reclaimable(wt, path) => {
                if apply {
                    let out = Command::new(tasks::git_bin())
                        .args(["worktree", "remove", &wt.to_string_lossy()])
                        .current_dir(root)
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {
                            report.items.push(PrunedItem {
                                path: path.clone(),
                                status: "reclaimed",
                                detail: format!("reclaimed: {}", path.display()),
                            });
                            report.reclaimed += 1;
                        }
                        _ => {
                            report.items.push(PrunedItem {
                                path: path.clone(),
                                status: "failed",
                                detail: format!("failed to remove: {}", path.display()),
                            });
                            report.failed += 1;
                        }
                    }
                } else {
                    report.items.push(PrunedItem {
                        path: path.clone(),
                        status: "reclaimable",
                        detail: format!("reclaimable: {}", path.display()),
                    });
                }
            }
        }
    }

    // Admin entries for directories that are already gone: housekeeping a
    // report run does too, as the script did; nothing of a worktree that
    // exists is touched by it.
    let _ = git(root, &["worktree", "prune"]);

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git args` in `dir`, with a test identity and none of the caller's
    /// repository settings.
    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new(tasks::git_bin())
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A primary checkout `<dir>/susi` with an origin, whose origin/main
    /// carries `.githooks/pre-push` and `scripts/seed-target.sh` (the stub for
    /// `susi workflow seed-target`; what the seeding does is Rust's, in
    /// process, so the file's text is nothing a test can observe).
    fn primary_with_seeder(dir: &Path) -> PathBuf {
        let server = dir.join("server.git");
        let primary = dir.join("susi");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        std::fs::create_dir_all(primary.join(".githooks")).unwrap();
        git_ok(&server, &["init", "--bare", "--quiet", "-b", "main"]);
        git_ok(&primary, &["init", "--quiet", "-b", "main"]);
        git_ok(&primary, &["config", "commit.gpgsign", "false"]);
        git_ok(
            &primary,
            &["remote", "add", "origin", server.to_str().unwrap()],
        );
        std::fs::write(
            primary.join(".githooks").join("pre-push"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                primary.join(".githooks/pre-push"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(primary.join(".gitignore"), "/target\n").unwrap();
        std::fs::write(
            primary.join("scripts").join("seed-target.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        git_ok(&primary, &["add", "-A"]);
        git_ok(&primary, &["commit", "--quiet", "-m", "init"]);
        git_ok(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        primary
    }

    /// `workflow start` no longer links the primary checkout's target, even
    /// when there is one to link (what v0.21.23 did): it seeds the new
    /// worktree's own, in process, given the worktree. With no snapshot to
    /// copy (none beside this fixture's primary) that is a real, empty target
    /// of the worktree's own, and the primary's is left as it was. A
    /// directory that is no checkout is left alone.
    #[test]
    fn a_new_worktree_is_seeded_by_its_own_script_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        let primary = primary_with_seeder(dir.path());
        // The shared target v0.21.23 linked every new worktree to.
        std::fs::create_dir_all(primary.join("target").join("debug")).unwrap();
        std::fs::write(primary.join("target").join("sentinel"), "the primary's").unwrap();
        let made = start_worktree(&primary, Some("worker"), Some("tester"), None).unwrap();
        let target = made.dest.join("target");
        let meta = target
            .symlink_metadata()
            .expect("start_worktree must give the new worktree a target of its own");
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "nothing links the primary's target"
        );
        assert!(
            !target.join("sentinel").exists(),
            "the worktree's target is not the primary's"
        );
        assert_eq!(
            std::fs::read_to_string(primary.join("target").join("sentinel")).unwrap(),
            "the primary's"
        );
        assert_eq!(
            std::fs::read_dir(primary.join("target")).unwrap().count(),
            2,
            "the primary's target is left exactly as it was"
        );
        // Outside any checkout, nothing runs and nothing is made.
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        seed_target(&bare);
        assert!(std::fs::read_dir(&bare).unwrap().next().is_none());
    }

    /// The copy takes about a minute; a worktree with no owner for that long
    /// is one a concurrent `workflow start` of the same name takes the reuse
    /// path into. So `start_worktree` seeds last: by the time the seeding
    /// runs, the worktree already names its agent and has its hooks. The
    /// seeding is in process now and observes nothing a test could read
    /// back, so the order is pinned where it is written.
    #[test]
    fn start_worktree_seeds_only_after_the_worktree_has_an_owner() {
        let text = include_str!("workflow.rs");
        let start = text
            .find("pub fn start_worktree(")
            .expect("start_worktree is defined here");
        let body = &text[start..];
        let body = &body[..body.find("\n}\n").expect("start_worktree ends")];
        let owner = body
            .find("\"susi.agent\", &worker")
            .expect("the worktree is given its owner");
        let hooks = body
            .find("ensure_infrastructure(&dest)")
            .expect("the worktree is given its hooks");
        let seeded = body
            .find("seed_target(&dest)")
            .expect("the worktree is seeded");
        assert!(
            owner < seeded && hooks < seeded,
            "the worktree must have its owner and hooks before it is seeded"
        );
        // And the seeding runs in the background: the start never waits for
        // the copy.
        assert!(text.contains("seed::seed_worktree(Some(dest), true)"));
    }

    fn good() -> Facts {
        Facts {
            root: "/w/feature".into(),
            primary: false,
            cloud: false,
            branch: Some("claude/feature".into()),
            behind: Some(0),
            hooks: Hooks {
                configured: Some(".githooks".into()),
                points_here: true,
                missing: vec![],
            },
            agent: "CLAUDE".into(),
            release: ReleaseDrift::default(),
            owed: Ok(vec![]),
            claims: Ok(vec![Claim {
                branch: None,
                scopes: vec![],
                task: "T-CLAUDE-1".into(),
                agent: "CLAUDE".into(),
                claimed_unix: 100,
                // Well inside the lease: `now` is 500, so this is ~2.6h left.
                lease_until_unix: 10_000,
            }]),
            porcelain: Some(String::new()),
            merging: false,
            watcher_age: Some(4),
            stale_mine: 0,
            stale_others: 0,
            now: 500,
        }
    }

    fn state(checks: &[Check], name: &str) -> State {
        checks.iter().find(|c| c.name == name).unwrap().state
    }

    #[test]
    fn a_ready_agent_passes_everything() {
        let c = evaluate(&good());
        assert!(ok(&c));
        assert!(
            c.iter().all(|x| x.state == State::Pass && x.fix.is_empty()),
            "{c:?}"
        );
    }

    #[test]
    fn a_cloud_sessions_primary_checkout_on_its_branch_is_its_own_worktree() {
        let mut f = good();
        f.primary = true;
        f.cloud = true;
        f.branch = Some("ccr-c9f023d1-sdljmo".into());
        f.watcher_age = None;
        let c = evaluate(&f);
        assert!(ok(&c), "{c:?}");
        assert_eq!(state(&c, "own worktree"), State::Pass);
        assert!(c[0].detail.contains("cloud session"), "{}", c[0].detail);
        // No primary checkout to keep converging: no watcher is asked for.
        assert_eq!(state(&c, "primary watcher"), State::Pass);
        // main and a detached HEAD are nobody's place, cloud or not.
        f.branch = Some("main".into());
        assert_eq!(state(&evaluate(&f), "own worktree"), State::Fail);
        f.branch = None;
        assert_eq!(state(&evaluate(&f), "own worktree"), State::Fail);
    }

    #[test]
    fn without_the_cloud_mark_the_primary_checkout_is_still_refused() {
        let mut f = good();
        f.primary = true;
        f.watcher_age = None;
        let c = evaluate(&f);
        assert_eq!(state(&c, "own worktree"), State::Fail);
        assert!(
            c[0].fix.contains("scripts/cloud-session.sh --mark"),
            "{}",
            c[0].fix
        );
        assert_eq!(state(&c, "primary watcher"), State::Warn);
    }

    #[test]
    fn primary_checkout_main_and_detached_head_are_not_your_own_worktree() {
        let mut f = good();
        f.primary = true;
        let c = evaluate(&f);
        assert_eq!(state(&c, "own worktree"), State::Fail);
        assert!(c[0].fix.contains("susi-worktree.sh"));
        f.primary = false;
        f.branch = Some("main".into());
        assert_eq!(state(&evaluate(&f), "own worktree"), State::Fail);
        f.branch = None;
        assert_eq!(state(&evaluate(&f), "own worktree"), State::Fail);
    }

    #[test]
    fn behind_and_unverified_freshness_fail() {
        let mut f = good();
        f.behind = Some(12);
        let c = evaluate(&f);
        assert_eq!(state(&c, "up to date"), State::Fail);
        assert!(c[1].detail.contains("12") && c[1].fix.contains("git merge origin/main"));
        f.behind = None;
        let c = evaluate(&f);
        assert_eq!(state(&c, "up to date"), State::Fail);
        assert!(!ok(&c), "freshness must be verified before work");
    }

    #[test]
    fn hooks_must_exist_and_be_this_worktrees_own() {
        let mut f = good();
        f.hooks.missing = vec!["commit-msg".into()];
        let c = evaluate(&f);
        assert_eq!(state(&c, "hooks installed"), State::Fail);
        assert!(c[2].detail.contains("commit-msg"));
        f.hooks.missing.clear();
        f.hooks.points_here = false;
        f.hooks.configured = Some("/elsewhere/.githooks".into());
        let c = evaluate(&f);
        assert_eq!(state(&c, "hooks installed"), State::Fail);
        assert!(c[2].detail.contains("/elsewhere/.githooks"));
    }

    #[test]
    fn a_claim_must_be_yours_and_live() {
        let mut f = good();
        f.claims = Ok(vec![Claim {
            branch: None,
            scopes: vec![],
            task: "T-DEVIN-2".into(),
            agent: "DEVIN".into(),
            claimed_unix: 100,
            lease_until_unix: 1_000,
        }]);
        assert_eq!(
            state(&evaluate(&f), "holds a claim"),
            State::Fail,
            "someone else's claim"
        );
        f.claims = Ok(vec![Claim {
            branch: None,
            scopes: vec![],
            task: "T-CLAUDE-1".into(),
            agent: "CLAUDE".into(),
            claimed_unix: 100,
            lease_until_unix: 400,
        }]);
        assert_eq!(
            state(&evaluate(&f), "holds a claim"),
            State::Fail,
            "expired lease"
        );
        f.claims = Ok(vec![]);
        assert_eq!(state(&evaluate(&f), "holds a claim"), State::Fail);
        f.claims = Err("no remote".into());
        let c = evaluate(&f);
        assert_eq!(state(&c, "holds a claim"), State::Fail);
        assert!(!ok(&c));
    }

    #[test]
    fn a_claim_on_another_branch_does_not_authorize_work() {
        let mut facts = good();
        facts.claims.as_mut().unwrap()[0].branch = Some("other-worker".into());
        assert_eq!(state(&evaluate(&facts), "holds a claim"), State::Fail);
    }

    #[test]
    fn every_failure_says_how_to_fix_it() {
        let f = Facts {
            primary: true,
            behind: Some(3),
            merging: true,
            hooks: Hooks {
                configured: None,
                points_here: false,
                missing: vec![],
            },
            claims: Ok(vec![]),
            ..good()
        };
        let c = evaluate(&f);
        for check in &c {
            if check.state == State::Fail {
                assert!(!check.fix.is_empty(), "{} needs a fix command", check.name);
            }
        }
        assert!(c.iter().any(|check| check.state == State::Fail));
    }

    #[test]
    fn accepted_tasks_awaiting_merge_warn_up_to_the_bound_and_fail_past_it() {
        let owed =
            |n: usize| -> Vec<String> { (7..7 + n).map(|i| format!("T-CLAUDE-{i}")).collect() };
        let cap = tasks::merges_in_flight();
        // Merges in flight are the pipeline: a warning to watch, not a blocker.
        let f = Facts {
            owed: Ok(owed(cap)),
            ..good()
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "merge published"), State::Warn);
        assert!(ok(&c), "merges in flight must not block the loop: {c:?}");

        // One past the bound is the pileup the receipts exist to refuse.
        let f = Facts {
            owed: Ok(owed(cap + 1)),
            ..good()
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "merge published"), State::Fail);
        assert!(
            !ok(&c),
            "acceptances piling up unpublished is not a working pipeline: {c:?}"
        );
        assert!(
            c.iter()
                .any(|x| x.fix.contains("susi workflow finish T-CLAUDE-7")
                    && x.fix.contains("--abandon")),
            "{c:?}"
        );
        // A receipt that cannot be read is a failure, never a silent pass:
        // "unknown" must not be mistaken for "merged".
        let offline = Facts {
            owed: Err("offline".into()),
            ..good()
        };
        assert_eq!(state(&evaluate(&offline), "merge published"), State::Fail);
    }

    #[test]
    fn uncommitted_work_warns_without_blocking() {
        let f = Facts {
            porcelain: Some(" M src/a.rs\n?? notes.md\n".into()),
            ..good()
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "tree clean"), State::Warn);
        assert!(ok(&c), "a work in progress must not block the loop: {c:?}");
        assert!(
            c.iter()
                .find(|x| x.name == "tree clean")
                .is_some_and(|x| x.detail.contains("2 uncommitted change(s)")),
            "{c:?}"
        );
    }

    #[test]
    fn an_unresolved_merge_fails_and_says_how_to_get_out() {
        let f = Facts {
            porcelain: Some("UU src/a.rs\n".into()),
            merging: true,
            ..good()
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "tree clean"), State::Fail);
        assert!(!ok(&c));
        assert!(
            c.iter().any(|x| x.fix.contains("git merge --abort")),
            "{c:?}"
        );
    }

    #[test]
    fn a_lease_about_to_lapse_is_reported_before_another_agent_takes_it() {
        let f = Facts {
            claims: Ok(vec![Claim {
                branch: None,
                scopes: vec![],
                task: "T-CLAUDE-1".into(),
                agent: "CLAUDE".into(),
                claimed_unix: 100,
                lease_until_unix: 500 + 60,
            }]),
            ..good()
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "lease healthy"), State::Warn);
        assert!(ok(&c));
        assert!(
            c.iter().any(|x| x.fix == "susi tasks renew T-CLAUDE-1"),
            "{c:?}"
        );
    }

    #[test]
    fn a_dead_primary_watcher_is_reported() {
        let stale = Facts {
            watcher_age: Some(9_000),
            ..good()
        };
        assert_eq!(state(&evaluate(&stale), "primary watcher"), State::Warn);
        let never = Facts {
            watcher_age: None,
            ..good()
        };
        let c = evaluate(&never);
        assert_eq!(state(&c, "primary watcher"), State::Warn);
        assert!(c.iter().any(|x| x.fix.contains("susi workflow watch")));
        assert!(ok(&c), "a dead watcher warns; it does not block work");
    }

    #[test]
    fn a_stale_installed_release_is_reported_but_never_blocks() {
        let mut f = good();
        f.release = ReleaseDrift {
            installed: true,
            commit: Some("ee0f8269efdecc14623cce7b2f5d48b913ec3797".into()),
            behind: Some(92),
            off_main: false,
            this_process: true,
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "installed susi"), State::Warn);
        let check = c
            .iter()
            .find(|x| x.name == "installed susi")
            .unwrap_or_else(|| panic!("no installed-susi check in {c:?}"));
        assert!(
            check.detail.contains("92 commit(s) behind origin/main"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("ee0f8269"), "{}", check.detail);
        assert!(check.fix.contains("susi-release-sync.sh"), "{}", check.fix);
        // Only the host owner can cut and promote a release; the loop runs on.
        assert!(ok(&c), "a stale release must not block work");
    }

    #[test]
    fn a_current_release_passes_and_a_dev_host_says_nothing() {
        // No marker (a dev build or a fresh host): nothing to compare.
        let mut f = good();
        let c = evaluate(&f);
        assert_eq!(state(&c, "installed susi"), State::Pass);
        assert!(
            c.iter()
                .any(|x| x.name == "installed susi" && x.detail.contains("no release marker")),
            "{c:?}"
        );

        f.release = ReleaseDrift {
            installed: true,
            commit: Some("abc1234def5678".into()),
            behind: Some(0),
            off_main: false,
            this_process: false,
        };
        let c = evaluate(&f);
        assert_eq!(state(&c, "installed susi"), State::Pass);
        assert!(
            c.iter()
                .any(|x| x.detail.contains("abc1234d") && x.detail.ends_with("is on origin/main")),
            "{c:?}"
        );

        // Built off main entirely: unknown distance, never a pass.
        f.release.off_main = true;
        f.release.behind = None;
        let c = evaluate(&f);
        assert_eq!(state(&c, "installed susi"), State::Warn);
        assert!(
            c.iter().any(|x| x.detail.contains("not on origin/main")),
            "{c:?}"
        );
    }

    #[test]
    fn finished_worktrees_are_reported_but_never_block() {
        // Yours: a warning with the command that reclaims them.
        let mine = Facts {
            stale_mine: 3,
            stale_others: 1,
            ..good()
        };
        let c = evaluate(&mine);
        assert_eq!(state(&c, "stale worktrees"), State::Warn);
        assert!(ok(&c), "housekeeping must not block the loop");
        assert!(
            c.iter()
                .any(|x| x.fix == "scripts/prune-worktrees.sh --apply"),
            "{c:?}"
        );
        assert!(
            c.iter()
                .any(|x| x.detail.contains("3 of your finished worktree(s)")),
            "{c:?}"
        );
        // Only someone else's: informational, not a warning.
        let theirs = Facts {
            stale_mine: 0,
            stale_others: 4,
            ..good()
        };
        let c = evaluate(&theirs);
        assert_eq!(state(&c, "stale worktrees"), State::Pass);
        assert!(c
            .iter()
            .any(|x| x.detail.contains("belong to other agents")));
        // None at all.
        assert_eq!(state(&evaluate(&good()), "stale worktrees"), State::Pass);
    }

    #[test]
    fn prune_worktrees_keeps_active_claimed_worktrees_and_reclaims_finished_ones() {
        let dir = tempfile::tempdir().unwrap();
        let primary = primary_with_seeder(dir.path());
        let active = start_worktree(&primary, Some("active"), Some("agent1"), None).unwrap();
        let finished = start_worktree(&primary, Some("done"), Some("agent2"), None).unwrap();

        let task = tasks::add(
            &active.dest,
            "agent1",
            tasks::NewTask {
                title: "active job".into(),
                goal: String::new(),
                size: "s".into(),
                deps: vec![],
                accept: vec!["cargo".into(), "--version".into()],
                roadmap: None,
            },
        )
        .unwrap();
        git_ok(&active.dest, &["add", "-A"]);
        git_ok(&active.dest, &["commit", "--quiet", "-m", "add task"]);
        git_ok(&active.dest, &["push", "--quiet", "origin", "HEAD:main"]);
        git_ok(&active.dest, &["fetch", "--quiet", "origin"]);
        tasks::claim_scoped(
            &active.dest,
            &task.id,
            &active.agent,
            tasks::ClaimOptions {
                hours: 1,
                now: tasks::now_unix(),
                scopes: &[],
            },
        )
        .unwrap();

        let report = prune_worktrees(&primary, true, true, "PRIMARY").unwrap();
        assert_eq!(report.reclaimed, 1, "{report:?}");
        assert!(
            active.dest.is_dir(),
            "active claimed worktree must never be pruned"
        );
        assert!(
            !finished.dest.is_dir(),
            "finished unclaimed worktree must be reclaimed"
        );
        assert!(report
            .items
            .iter()
            .any(|item| item.path == canonical(&active.dest) && item.status == "claimed"));
    }
}
