//! Every fix `susi workflow check` prints must be actionable.
//!
//! The check exists to tell an agent *exactly* what to run. That guidance is
//! only worth printing if it is real: a script that ships in this repository
//! and is executable, or a command whose tool exists. This file previously
//! defined its own `FixPlan` and asserted on that — a test of the test, for a
//! `susi workflow check --fix` flag that has never existed — so it could not
//! fail however the guidance rotted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str], home: &Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

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

/// A clone that is *not* ready: no hooks, no claim, no watcher heartbeat, and a
/// dirty tree — so several failures (and warnings) print their fix at once.
struct World {
    root: PathBuf,
    primary: PathBuf,
    worktree: PathBuf,
    home: PathBuf,
}

impl World {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-zcfix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("primary");
        let home = root.join("home");
        for dir in [&bare, &primary, &home] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git(
            &primary,
            &["commit", "--allow-empty", "--quiet", "-m", "init"],
        );
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        let worktree = root.join("wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "work",
                worktree.to_str().unwrap(),
                "origin/main",
            ],
        );
        std::fs::write(worktree.join("scratch.txt"), "uncommitted").unwrap();
        Self {
            root,
            primary,
            worktree,
            home,
        }
    }

    fn check(&self, dir: &Path) -> (i32, String) {
        run(dir, &["workflow", "check"], &self.home)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fix_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("→ "))
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(_path: &Path) -> bool {
    true
}

#[test]
fn every_fix_the_check_prints_is_actionable() {
    let w = World::new();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));

    // The worktree, and the primary checkout (which also prints the "make
    // yourself a worktree" fix).
    for dir in [&w.worktree, &w.primary] {
        let (_code, out) = w.check(dir);
        let fixes = fix_lines(&out);
        assert!(
            !fixes.is_empty(),
            "a not-ready checkout must print fixes ({}):\n{out}",
            dir.display()
        );
        for fix in &fixes {
            let first = fix.split_whitespace().next().unwrap_or_default();
            if matches!(first, "git" | "susi" | "cargo") {
                continue;
            }
            // Anything else must be a script that ships in this repository and
            // can actually be run. This is what rots when a script is renamed.
            let path = repo.join(first);
            assert!(
                path.is_file(),
                "fix names a file that does not exist: `{fix}` (looked for {})",
                path.display()
            );
            assert!(
                executable(&path),
                "fix names a script that is not executable: `{fix}`"
            );
        }
    }
}

#[test]
fn the_check_does_not_advertise_a_flag_it_lacks() {
    // The guidance may only mention flags the CLI accepts: this file used to
    // promise `susi workflow check --fix`, which has never existed.
    let w = World::new();
    let (_, out) = w.check(&w.worktree);
    assert!(
        !out.contains("--fix"),
        "check advertises --fix, which the CLI does not implement:\n{out}"
    );
}
