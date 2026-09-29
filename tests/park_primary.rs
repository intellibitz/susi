#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/park-primary.sh`: the primary checkout is detached at origin/main,
//! and left alone (with the reason) when it holds real work.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
}

impl World {
    /// bare server; primary on a stale branch one commit behind origin/main.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-park-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("primary");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&primary).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git(
            &primary,
            &["commit", "--allow-empty", "--quiet", "-m", "one"],
        );
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["switch", "--quiet", "-c", "stale-branch"]);
        // origin/main moves on.
        let other = root.join("other");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        git(&other, &["commit", "--allow-empty", "--quiet", "-m", "two"]);
        git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        Self { root, primary }
    }

    fn park(&self, from: &Path) -> (i32, String, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/park-primary.sh");
        run(from, script.to_str().unwrap(), &[])
    }

    fn head_is_detached_at_origin_main(&self) -> bool {
        let detached = run(&self.primary, "git", &["symbolic-ref", "-q", "HEAD"]).0 != 0;
        detached
            && git(&self.primary, &["rev-parse", "HEAD"])
                == git(&self.primary, &["rev-parse", "origin/main"])
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_clean_stale_primary_is_detached_at_origin_main() {
    let w = World::new("clean");
    let (code, out, err) = w.park(&w.primary);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("moved from stale-branch"), "{out}");
    assert!(w.head_is_detached_at_origin_main());
    // Idempotent.
    let (code, out, _) = w.park(&w.primary);
    assert_eq!(code, 0);
    assert!(out.contains("already parked"), "{out}");
}

#[test]
fn it_can_be_run_from_a_linked_worktree() {
    let w = World::new("wt");
    let wt = w.root.join("wt");
    git(
        &w.primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            wt.to_str().unwrap(),
            "origin/main",
        ],
    );
    let (code, _, err) = w.park(&wt);
    assert_eq!(code, 0, "{err}");
    assert!(w.head_is_detached_at_origin_main());
}

#[test]
fn uncommitted_work_blocks_parking_and_is_untouched() {
    let w = World::new("dirty");
    std::fs::write(w.primary.join("wip.txt"), "unsaved").unwrap();
    let (code, _, err) = w.park(&w.primary);
    assert_eq!(code, 1);
    assert!(
        err.contains("uncommitted changes") && err.contains("wip.txt"),
        "{err}"
    );
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "stale-branch"
    );
    assert!(w.primary.join("wip.txt").exists());
}

#[test]
fn commits_not_in_origin_main_block_parking() {
    let w = World::new("unmerged");
    git(
        &w.primary,
        &["commit", "--allow-empty", "--quiet", "-m", "local only"],
    );
    let (code, _, err) = w.park(&w.primary);
    assert_eq!(code, 1);
    assert!(
        err.contains("1 commit(s) on stale-branch are not in origin/main"),
        "{err}"
    );
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "stale-branch"
    );
}
