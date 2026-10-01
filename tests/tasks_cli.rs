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

    let (code, _, err) = susi(&a, &home, "claude", &["tasks", "claim", "T-CLAUDE-1"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = susi(&b, &home, "devin", &["tasks", "claim", "T-CLAUDE-1"]);
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
    let (code, _, err) = susi(&b, &home, "devin", &["tasks", "claim", "T-CLAUDE-1"]);
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
