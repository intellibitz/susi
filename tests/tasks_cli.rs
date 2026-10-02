#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi tasks` end to end through the real binary: add → claim (atomic,
//! second claimant refused) → close only when the acceptance check passes.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn susi(dir: &Path, home: &Path, agent: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("SUSI_AGENT", agent)
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn clone_of(bare: &Path, root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet"]);
    git(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&dir, &["commit", "--allow-empty", "-m", "init", "--quiet"]);
    dir
}

#[test]
fn add_claim_race_and_gated_close_through_the_binary() {
    let root = std::env::temp_dir().join(format!("susi-tasks-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bare = root.join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let a = clone_of(&bare, &root, "a");
    let b = clone_of(&bare, &root, "b");
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&b, &["fetch", "--quiet", "origin"]);
    git(&b, &["reset", "--hard", "origin/main"]);

    let (code, out, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "add", "prove it", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-CLAUDE-1"), "{out}");
    std::fs::create_dir_all(b.join(".agents/tasks")).unwrap();
    std::fs::copy(
        a.join(".agents/tasks/T-CLAUDE-1.json"),
        b.join(".agents/tasks/T-CLAUDE-1.json"),
    )
    .unwrap();

    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "claim", "T-CLAUDE-1", "--scope", "work"],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = susi(
        &b,
        &home,
        "devin",
        // Disjoint from claude's reservation, so what fails is the ownership of
        // the task itself — the point of this test — rather than the overlap.
        &["tasks", "claim", "T-CLAUDE-1", "--scope", "devin-area"],
    );
    assert_ne!(code, 0);
    assert!(err.contains("claimed by CLAUDE"), "{err}");

    let (_, listing, _) = susi(&b, &home, "devin", &["tasks"]);
    assert!(listing.contains("\"claimed_by\": \"CLAUDE\""), "{listing}");

    // A failing acceptance command keeps the task open.
    let bad = a.join(".agents/tasks/T-CLAUDE-1.json");
    let original = std::fs::read_to_string(&bad).unwrap();
    std::fs::write(
        &bad,
        original.replace("--version", "definitely-not-a-subcommand"),
    )
    .unwrap();
    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "close", "T-CLAUDE-1"]);
    assert_ne!(code, 0);
    assert!(err.contains("not done"), "{err}");
    assert!(bad.exists());

    std::fs::write(&bad, original).unwrap();
    let (code, out, err) = susi(&a, &home, "claude", &["tasks", "close", "T-CLAUDE-1"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("closed T-CLAUDE-1"), "{out}");
    assert!(!bad.exists());
    assert!(a.join(".agents/tasks/done/T-CLAUDE-1.json").exists());
    // Ownership remains while the completion awaits publication.
    let (_, listing, _) = susi(&b, &home, "devin", &["tasks"]);
    assert!(listing.contains("CLAUDE\""), "{listing}");
    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "release", "T-CLAUDE-1"]);
    assert_eq!(code, 0, "{err}");
    let (_, listing, _) = susi(&b, &home, "devin", &["tasks"]);
    assert!(!listing.contains("CLAUDE\""), "{listing}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Closing is judged against `origin/main`, not only the worktree. An agent that
/// claimed a task before another branch's close merged, and closed it afterwards
/// without re-syncing, wrote a second `done/<id>.json` on divergent history —
/// four ids in this repo's history were closed twice that way, and there is no
/// merge driver for `.agents/tasks/**`, so the conflict is resolved by hand and
/// one record silently wins.
#[test]
fn a_task_already_closed_on_main_cannot_be_closed_again() {
    let root = std::env::temp_dir().join(format!("susi-tasks-double-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bare = root.join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let a = clone_of(&bare, &root, "a");
    let b = clone_of(&bare, &root, "b");
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&b, &["fetch", "--quiet", "origin"]);
    git(&b, &["reset", "--hard", "origin/main"]);

    // `a` publishes the task; `b` syncs and takes the claim.
    let (code, out, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "add", "shared", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-CLAUDE-1"), "{out}");
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "--quiet", "-m", "add task"]);
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&b, &["fetch", "--quiet", "origin"]);
    git(&b, &["merge", "--quiet", "--no-edit", "origin/main"]);
    let (code, _, err) = susi(
        &b,
        &home,
        "devin",
        &["tasks", "claim", "T-CLAUDE-1", "--scope", "work"],
    );
    assert_eq!(code, 0, "{err}");

    // Meanwhile the task is closed on main by another route and `b` has not
    // re-synced, so its worktree still shows the task open and it holds a live
    // claim — precisely the window that produced the duplicate closes.
    git(&a, &["fetch", "--quiet", "origin"]);
    std::fs::create_dir_all(a.join(".agents/tasks/done")).unwrap();
    git(
        &a,
        &[
            "mv",
            ".agents/tasks/T-CLAUDE-1.json",
            ".agents/tasks/done/T-CLAUDE-1.json",
        ],
    );
    git(&a, &["commit", "--quiet", "-m", "close the task elsewhere"]);
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);

    assert!(b.join(".agents/tasks/T-CLAUDE-1.json").exists());
    let (code, _, err) = susi(&b, &home, "devin", &["tasks", "close", "T-CLAUDE-1"]);
    assert_ne!(code, 0, "a second close must be refused: {err}");
    assert!(err.contains("already closed on origin/main"), "{err}");
    // Refused before any second record was written.
    assert!(b.join(".agents/tasks/T-CLAUDE-1.json").exists());
    assert!(!b.join(".agents/tasks/done/T-CLAUDE-1.json").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// Runs the binary without `SUSI_AGENT`, so the identity falls through to the
/// worktree and the process tree exactly as it does for a real worker.
fn susi_no_agent(dir: &Path, home: &Path, program: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_AGENT")
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A worktree with no `susi.agent` and a branch that does not name an agent
/// used to fall back to the clone-wide `user.name`: the primary checkout, a
/// codex tree and a claude tree all resolved to the same token, sharing one
/// `refs/claim-agents/<TOKEN>` and one task-id namespace. The tool the agent
/// runs inside now names it — without inventing one when nothing matches.
#[test]
fn the_tool_in_the_process_tree_names_the_agent() {
    if !Path::new("/proc/self/stat").exists() {
        return; // No /proc: the fallback is unchanged, nothing to assert.
    }
    let root = std::env::temp_dir().join(format!("susi-tasks-ident-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.name", "SHAREDTOKEN"]);
    git(&repo, &["config", "user.email", "shared@localhost"]);
    git(&repo, &["commit", "--allow-empty", "--quiet", "-m", "init"]);

    // Nothing in the tree names an agent, and this shell names no tool either.
    let (_, out, _) = susi_no_agent(
        &repo,
        &home,
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "check"],
    );
    assert!(out.contains("(agent SHAREDTOKEN)"), "{out}");

    // Inside a wrapper that is the tool, the process tree names the agent.
    let wrapper = root.join("cursor-agent");
    std::fs::write(
        &wrapper,
        format!("#!/bin/bash\n\"{}\" \"$@\"\n", env!("CARGO_BIN_EXE_susi")),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_, out, err) = susi_no_agent(
        &repo,
        &home,
        wrapper.to_str().unwrap(),
        &["workflow", "check"],
    );
    assert!(out.contains("(agent CURSOR)"), "out={out} err={err}");

    // The same wrapper without the tool's name keeps the old fallback.
    let plain = root.join("worker");
    std::fs::write(
        &plain,
        format!("#!/bin/bash\n\"{}\" \"$@\"\n", env!("CARGO_BIN_EXE_susi")),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_, out, _) = susi_no_agent(
        &repo,
        &home,
        plain.to_str().unwrap(),
        &["workflow", "check"],
    );
    assert!(out.contains("(agent SHAREDTOKEN)"), "{out}");

    let _ = std::fs::remove_dir_all(&root);
}

/// A claim that reserves no paths cannot be enforced — the commit hook and CI
/// refuse any code under it — so refusing it at claim time is the difference
/// between an immediate, obvious error and one that appears when the agent
/// tries to commit its work.
#[test]
fn a_claim_without_reserved_paths_is_refused_unless_deliberate() {
    let root = std::env::temp_dir().join(format!("susi-tasks-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bare = root.join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let a = clone_of(&bare, &root, "a");
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "add", "job", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");

    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "claim", "T-CLAUDE-1"]);
    assert_ne!(code, 0, "a scopeless claim must be refused");
    assert!(err.contains("would reserve no paths"), "{err}");
    assert!(err.contains("--scope"), "{err}");
    assert!(err.contains("--unscoped"), "{err}");
    // Refused before anything was reserved.
    let (_, listing, _) = susi(&a, &home, "claude", &["tasks"]);
    assert!(!listing.contains("claimed_by\": \"CLAUDE"), "{listing}");

    // The deliberate override still claims it.
    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "claim", "T-CLAUDE-1", "--unscoped"],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "release", "T-CLAUDE-1"]);
    assert_eq!(code, 0, "{err}");

    // And with a scope, as the loop intends.
    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "claim", "T-CLAUDE-1", "--scope", "src/cli"],
    );
    assert_eq!(code, 0, "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A size-l task is integration work that will not finish inside the default
/// lease, and nothing renews a claim while an agent is at work — an expired
/// lease is taken over, handing the same task to a second agent mid-flight.
#[test]
fn a_large_task_is_claimed_with_a_lease_that_outlives_the_work() {
    let root = std::env::temp_dir().join(format!("susi-tasks-lease-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bare = root.join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let a = clone_of(&bare, &root, "a");
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &[
            "tasks",
            "add",
            "big",
            "--accept",
            "cargo --version",
            "--size",
            "l",
        ],
    );
    assert_eq!(code, 0, "{err}");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (code, out, err) = susi(
        &a,
        &home,
        "claude",
        &["tasks", "claim", "T-CLAUDE-1", "--scope", "src"],
    );
    assert_eq!(code, 0, "{err}");
    let lease: u64 = out
        .rsplit("until unix ")
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("no lease in {out}"));
    let hours = (lease - now) / 3600;
    assert!(
        (11..=12).contains(&hours),
        "a size-l claim should hold ~12h, got {hours}h ({out})"
    );

    // An explicit --hours still wins.
    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "release", "T-CLAUDE-1"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = susi(
        &a,
        &home,
        "claude",
        &[
            "tasks",
            "claim",
            "T-CLAUDE-1",
            "--scope",
            "src",
            "--hours",
            "2",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let lease: u64 = out
        .rsplit("until unix ")
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        (lease - now) < 3 * 3600,
        "an explicit --hours must win: {out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The 30 verification tasks accept `scripts/check-roadmap-verdict.py <VC-id>`
/// — a script with an argument, which nothing had exercised end to end: the
/// argv split, the cwd, the exit-code reading and the zero-evidence guard were
/// all assumed. If it were wrong, all 30 would be unclosable.
#[test]
fn a_script_acceptance_with_arguments_closes_a_task() {
    let root = std::env::temp_dir().join(format!("susi-tasks-script-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bare = root.join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--quiet"]);
    let a = clone_of(&bare, &root, "a");
    git(&a, &["push", "--quiet", "origin", "HEAD:main"]);
    // The checker must exist in the checkout the acceptance runs from.
    std::fs::create_dir_all(a.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-roadmap-verdict.py"),
        a.join("scripts/check-roadmap-verdict.py"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            a.join("scripts/check-roadmap-verdict.py"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    // It passes (its own self-test), so the task closes with a done/ record.
    let (code, out, err) = susi(
        &a,
        &home,
        "claude",
        &[
            "tasks",
            "add",
            "verify a vector",
            "--accept",
            "scripts/check-roadmap-verdict.py --self-test",
            "--size",
            "l",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-CLAUDE-1"), "{out}");
    let (code, _, err) = susi(
        &a,
        &home,
        "claude",
        &[
            "tasks",
            "claim",
            "T-CLAUDE-1",
            "--scope",
            "crates/susi-gawd/src",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = susi(&a, &home, "claude", &["tasks", "close", "T-CLAUDE-1"]);
    assert_eq!(
        code, 0,
        "a passing script acceptance must close the task: {err}"
    );
    assert!(out.contains("closed T-CLAUDE-1"), "{out}");
    assert!(a.join(".agents/tasks/done/T-CLAUDE-1.json").exists());

    // An unrecorded vector exits 1, so the task stays open with its claim.
    let (code, _, err) = susi(
        &a,
        &home,
        "devin",
        &[
            "tasks",
            "add",
            "verify another",
            "--accept",
            "scripts/check-roadmap-verdict.py VC-201-999",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = susi(
        &a,
        &home,
        "devin",
        &[
            "tasks",
            "claim",
            "T-DEVIN-1",
            "--scope",
            "crates/susi-gemi/src",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = susi(&a, &home, "devin", &["tasks", "close", "T-DEVIN-1"]);
    assert_ne!(code, 0, "an unrecorded verdict must not close the task");
    // The closer sees the acceptance's own output, so a refusal explains itself
    // rather than leaving the agent to guess what the check wanted.
    assert!(
        out.contains("no verdict recorded"),
        "stdout={out} stderr={err}"
    );
    assert!(
        a.join(".agents/tasks/T-DEVIN-1.json").exists(),
        "a failed acceptance must leave the task open"
    );
    let _ = std::fs::remove_dir_all(&root);
}
