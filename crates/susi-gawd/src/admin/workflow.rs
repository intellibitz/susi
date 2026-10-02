//! `susi workflow check`: can this agent start work here, right now?
//!
//! The rules (Mandates 49-51) are only followed if an agent learns about them
//! *before* it edits. This gathers facts about the current checkout and turns
//! them into a checklist with the exact fix for each failure:
//!
//! 1. it is the agent's own worktree (not the primary checkout, not `main`,
//!    not a detached HEAD),
//! 2. it is up to date with `origin/main`,
//! 3. this worktree's git hooks are installed and are its own,
//! 4. the agent holds a live claim on a task,
//! 5. the tree can actually be synced (no merge in progress, nothing
//!    uncommitted — `sync` and `finish` both refuse a dirty tree, but nothing
//!    used to say so before they did),
//! 6. that claim's lease is not about to lapse,
//! 7. the primary-checkout watcher is alive (it is what keeps the primary
//!    parked at `origin/main`),
//! 8. the release installed on this host is not older than the fixes merged
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
    /// `None` = detached HEAD.
    pub branch: Option<String>,
    /// Commits `origin/main` has that HEAD lacks; `None` = could not fetch.
    pub behind: Option<u64>,
    pub hooks: Hooks,
    pub agent: String,
    /// Claims on the remote, or why they could not be read.
    pub claims: Result<Vec<Claim>, String>,
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

    // 1. Own worktree.
    out.push(if f.primary {
        fail(
            "own worktree",
            "this is the primary checkout; nobody works here (Mandate 49)",
            "scripts/susi-worktree.sh <name>   # then cd into the new worktree",
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

    // 5. Can this tree be synced at all? `sync` refuses a dirty tree and the
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

    // 6. A claim that is close to lapsing: finish renews while it waits, but a
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

    // 7. The primary checkout only converges while this clone's watcher runs
    //    (AGENTS.md requires it), and nothing noticed when it died.
    out.push(match f.watcher_age {
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
    });

    // 8. The binary the agent is running. `~/.susi/bin/susi` changes only when
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

    // 9. Finished worktrees accumulate in a clone — one per task, and nothing
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
    let out = Command::new("git")
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
    if git(root, &["config", "--get", "core.hooksPath"]).as_deref() != Some(".githooks") {
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
    if git(root, &["config", "--get", "merge.ledger.driver"]).is_none() {
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
                "python3 scripts/merge-ledger.py %O %A %B",
            ],
        );
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
    let root = git(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    let abs =
        |arg: &str| git(&root, &["rev-parse", "--path-format=absolute", arg]).map(PathBuf::from);
    let primary = match (abs("--git-dir"), abs("--git-common-dir")) {
        (Some(gd), Some(cd)) => canonical(&gd) == canonical(&cd),
        _ => false,
    };
    let branch = git(&root, &["symbolic-ref", "-q", "--short", "HEAD"]).filter(|b| !b.is_empty());

    // An unreachable remote leaves whatever origin/main we last saw; report
    // freshness as unknown rather than certifying a stale ref.
    let behind = git(&root, &["fetch", "--quiet", "origin"])
        .and_then(|_| git(&root, &["rev-list", "--count", "HEAD..origin/main"]))
        .and_then(|n| n.parse::<u64>().ok());

    let configured = git(&root, &["config", "--get", "core.hooksPath"]).filter(|s| !s.is_empty());
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
        .collect();

    let claims = tasks::claims(&root).map_err(|e| e.to_string());

    // What blocks `sync` and the gate, which used to be invisible here.
    let porcelain = git(&root, &["status", "--porcelain"]);
    let merging = ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
        .iter()
        .any(|m| git(&root, &["rev-parse", "--verify", "--quiet", m]).is_some())
        || ["rebase-merge", "rebase-apply"].iter().any(|d| {
            git(
                &root,
                &["rev-parse", "--path-format=absolute", "--git-path", d],
            )
            .is_some_and(|p| Path::new(&p).exists())
        });

    // The watcher writes this every loop; a stale stamp is how a dead watcher
    // becomes visible, since the log file stays empty while things go well.
    let now = tasks::now_unix();
    let watcher_age = abs("--git-common-dir")
        .map(|c| c.join("susi-primary-watch.stamp"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|stamp| now.saturating_sub(stamp));

    // Finished worktrees pile up: one per task, and nothing reclaims them.
    let common_dir = abs("--git-common-dir");
    let (stale_mine, stale_others) = match &common_dir {
        Some(dir) => {
            let primary_dir = Path::new(dir)
                .parent()
                .unwrap_or(Path::new("/"))
                .to_path_buf();
            finished_worktrees(&root, &primary_dir, agent)
        }
        None => (0, 0),
    };

    // The tool the agent runs is part of the loop's state: a release that
    // predates a merged fix enforces the older rules.
    let release = release_drift(&root);

    Facts {
        root,
        primary,
        branch,
        behind,
        hooks: Hooks {
            configured,
            points_here,
            missing,
        },
        agent: agent.to_string(),
        claims,
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
    // Only commits that changed the tree count. The merge that lands the
    // release branch on main adds no content, and counting it made every
    // release read as one commit stale the moment it landed — a warning that
    // is wrong the day it appears is one agents learn to ignore.
    let behind = (!off_main)
        .then(|| {
            git(
                root,
                &[
                    "rev-list",
                    "--no-merges",
                    "--count",
                    &format!("{commit}..origin/main"),
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
    let mut mine = 0;
    let mut others = 0;
    for path in listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
    {
        let resolved = canonical(&path);
        if resolved == primary || resolved == here {
            continue;
        }
        if !git(&path, &["status", "--porcelain"]).is_some_and(|s| s.trim().is_empty()) {
            continue;
        }
        let Some(head) = git(&path, &["rev-parse", "HEAD"]) else {
            continue;
        };
        // `--is-ancestor` succeeds only when the commit is contained.
        if git(
            &path,
            &["merge-base", "--is-ancestor", &head, "origin/main"],
        )
        .is_none()
        {
            continue;
        }
        let owner = git(&path, &["config", "--get", "susi.agent"]).unwrap_or_default();
        if !me.is_empty() && owner.eq_ignore_ascii_case(me) {
            mine += 1;
        } else {
            others += 1;
        }
    }
    (mine, others)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Facts {
        Facts {
            root: "/w/feature".into(),
            primary: false,
            branch: Some("claude/feature".into()),
            behind: Some(0),
            hooks: Hooks {
                configured: Some(".githooks".into()),
                points_here: true,
                missing: vec![],
            },
            agent: "CLAUDE".into(),
            release: ReleaseDrift::default(),
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
}
