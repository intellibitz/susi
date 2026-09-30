#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Mandate 49: any agent's git activity in a worktree keeps the primary
//! checkout's `main` current, through `.githooks/sync-primary`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .trim()
        .to_string(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out) = run(dir, args, &[]);
    assert_eq!(code, 0, "git {args:?}: {out}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    other: PathBuf,
}

impl World {
    /// bare server + primary on main (carrying the real scripts and sync
    /// hooks) + a worktree whose hooks are the sync hooks only.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-synchk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bare, primary, wt, other) = (
            root.join("server.git"),
            root.join("primary"),
            root.join("wt"),
            root.join("other"),
        );
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        std::fs::create_dir_all(primary.join(".githooks")).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (src, dst) in [
            ("scripts/park-primary.sh", "scripts/park-primary.sh"),
            (".githooks/sync-primary", ".githooks/sync-primary"),
            (".githooks/post-commit", ".githooks/post-commit"),
            (".githooks/post-merge", ".githooks/post-merge"),
            (".githooks/post-checkout", ".githooks/post-checkout"),
        ] {
            let to = primary.join(dst);
            std::fs::copy(repo.join(src), &to).unwrap();
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "work",
                wt.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(&wt, &["config", "core.hooksPath", ".githooks"]);
        git(
            &root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        Self {
            root,
            primary,
            wt,
            other,
        }
    }

    fn origin_advances(&self) {
        git(
            &self.other,
            &["commit", "--allow-empty", "--quiet", "-m", "newer"],
        );
        git(&self.other, &["push", "--quiet", "origin", "HEAD:main"]);
    }

    fn commit_in_worktree(&self, envs: &[(&str, &str)]) -> i32 {
        run(
            &self.wt,
            &["commit", "--allow-empty", "--quiet", "-m", "work"],
            envs,
        )
        .0
    }

    fn primary_head(&self) -> String {
        git(&self.primary, &["rev-parse", "HEAD"])
    }

    fn server_main(&self) -> String {
        git(&self.other, &["rev-parse", "origin/main"])
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const FG: (&str, &str) = ("SUSI_SYNC_FOREGROUND", "1");

#[test]
fn hooks_sync_primary_after_a_commit_in_any_worktree() {
    let w = World::new("commit");
    w.origin_advances();
    let before = w.primary_head();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert_ne!(w.primary_head(), before, "primary main should have moved");
    assert_eq!(
        w.primary_head(),
        git(&w.primary, &["rev-parse", "origin/main"])
    );
    assert_eq!(w.primary_head(), w.server_main());
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
}

#[test]
fn hooks_sync_primary_is_throttled_and_the_interval_is_configurable() {
    let w = World::new("throttle");
    w.origin_advances();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    let synced = w.primary_head();
    w.origin_advances();
    // Within the default 30s window the second commit does not fetch again.
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert_eq!(w.primary_head(), synced, "throttled: no second sync");
    assert_eq!(w.commit_in_worktree(&[FG, ("SUSI_SYNC_INTERVAL", "0")]), 0);
    assert_eq!(w.primary_head(), w.server_main(), "interval 0 syncs again");
}

#[test]
fn hooks_sync_primary_never_fails_or_moves_unsafe_state() {
    let w = World::new("safe");
    w.origin_advances();
    // Dirty primary: no move, and the commit still succeeds.
    std::fs::write(w.primary.join("scratch.txt"), "wip").unwrap();
    let before = w.primary_head();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert_eq!(w.primary_head(), before);
    // Server unreachable: the commit still succeeds.
    std::fs::remove_file(w.primary.join("scratch.txt")).unwrap();
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    assert_eq!(w.commit_in_worktree(&[FG, ("SUSI_SYNC_INTERVAL", "0")]), 0);
}

#[test]
fn hooks_sync_primary_is_wired_into_every_git_activity_hook() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks");
    for h in ["post-commit", "post-merge", "post-checkout", "pre-push"] {
        let text = std::fs::read_to_string(repo.join(h)).unwrap();
        assert!(text.contains("sync-primary"), "{h} must call sync-primary");
        let mode = std::fs::metadata(repo.join(h))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{h} must be executable");
    }
}
