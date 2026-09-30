#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi workflow check` against real git topologies: a primary checkout, a
//! worktree, a stale clone, missing hooks and missing claims.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("SUSI_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let (code, _, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
}

struct World {
    root: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    home: PathBuf,
}

impl World {
    /// bare server + a primary clone with `.githooks` on main + a worktree.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-wfc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("primary");
        let home = root.join("home");
        for d in [&bare, &primary, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        std::fs::create_dir_all(primary.join(".githooks")).unwrap();
        for hook in ["commit-msg", "pre-commit", "pre-push", "workflow-guard"] {
            let p = primary.join(".githooks").join(hook);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        let park = primary.join("scripts/park-primary.sh");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/park-primary.sh"),
            &park,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&park, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        let wt = root.join("wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "work",
                wt.to_str().unwrap(),
            ],
        );
        git(&wt, &["config", "core.hooksPath", ".githooks"]);
        Self {
            root,
            primary,
            wt,
            home,
        }
    }

    fn check(&self, dir: &Path) -> (i32, String) {
        let (code, out, _) = run(
            dir,
            env!("CARGO_BIN_EXE_susi"),
            &["workflow", "check"],
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        );
        (code, out)
    }

    fn susi(&self, dir: &Path, args: &[&str]) -> (i32, String, String) {
        run(
            dir,
            env!("CARGO_BIN_EXE_susi"),
            args,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        )
    }

    fn claim_a_task(&self) {
        let (code, _, err) = self.susi(
            &self.wt,
            &["tasks", "add", "job", "--accept", "cargo --version"],
        );
        assert_eq!(code, 0, "{err}");
        let (code, _, err) = self.susi(&self.wt, &["tasks", "claim", "T-TEST-1"]);
        assert_eq!(code, 0, "{err}");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_ready_worktree_passes_and_says_so() {
    let w = World::new("ready");
    w.claim_a_task();
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    for needle in [
        "✅ own worktree",
        "✅ up to date",
        "✅ hooks installed",
        "✅ holds a claim",
        "ready:",
    ] {
        assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
    }
}

#[test]
fn the_primary_checkout_is_refused_with_the_fix() {
    let w = World::new("primary");
    w.claim_a_task();
    let (code, out) = w.check(&w.primary);
    assert_ne!(code, 0);
    assert!(out.contains("❌ own worktree"), "{out}");
    assert!(out.contains("scripts/susi-worktree.sh"), "{out}");
}

#[test]
fn a_stale_worktree_is_told_to_merge_origin_main() {
    let w = World::new("stale");
    w.claim_a_task();
    // Someone else advances origin/main.
    let other = w.root.join("other");
    git(
        &w.root,
        &[
            "clone",
            "--quiet",
            w.root.join("server.git").to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "newer"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(out.contains("❌ up to date"), "{out}");
    assert!(out.contains("1 commit(s) behind origin/main"), "{out}");
    assert!(out.contains("git merge origin/main"), "{out}");
}

#[test]
fn missing_hooks_are_healed_and_missing_claim_is_reported() {
    let w = World::new("hooks");
    // No claim yet, and this worktree's hooks are not configured — check
    // heals hooksPath itself (hooks-on-clone), so the only ❌ left is
    // the missing claim.
    git(&w.wt, &["config", "--unset", "core.hooksPath"]);
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(out.contains("✅ hooks installed"), "{out}");
    assert_eq!(
        git_out(&w.wt, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    assert!(out.contains("❌ holds a claim"), "{out}");
    assert!(out.contains("susi tasks claim"), "{out}");
}

#[test]
fn hooks_missing_from_the_branch_are_named() {
    let w = World::new("nohooks");
    w.claim_a_task();
    std::fs::remove_file(w.wt.join(".githooks/commit-msg")).unwrap();
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(
        out.contains("missing or not executable: commit-msg"),
        "{out}"
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out.trim().to_string()
}

#[test]
fn workflow_check_keeps_the_primary_checkouts_main_current() {
    let w = World::new("sync");
    w.claim_a_task();
    // origin/main moves on; the primary checkout (on main) is now behind.
    let other = w.root.join("other");
    git(
        &w.root,
        &[
            "clone",
            "--quiet",
            w.root.join("server.git").to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "newer"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let before = git_out(&w.primary, &["rev-parse", "HEAD"]);
    // Running the check from a worktree advances the primary checkout.
    let (_, out) = w.check(&w.wt);
    let after = git_out(&w.primary, &["rev-parse", "HEAD"]);
    assert_ne!(
        before, after,
        "primary main should have been fast-forwarded:\n{out}"
    );
    assert_eq!(after, git_out(&w.primary, &["rev-parse", "origin/main"]));
    assert_eq!(
        git_out(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
}

fn advance_origin(w: &World) {
    let other = w.root.join("other");
    if !other.exists() {
        git(
            &w.root,
            &[
                "clone",
                "--quiet",
                w.root.join("server.git").to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
    }
    git(&other, &["pull", "--quiet", "origin", "main"]);
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "newer"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
}

fn add_task(w: &World) {
    let (code, _, err) = w.susi(
        &w.wt,
        &["tasks", "add", "job", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");
}

#[test]
fn claim_requires_a_synced_worktree_and_says_how_to_sync() {
    let w = World::new("claimsync");
    add_task(&w);
    advance_origin(&w);
    let (code, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert_ne!(code, 0, "a stale worktree must not claim");
    assert!(err.contains("1 commit(s) behind origin/main"), "{err}");
    assert!(err.contains("git merge origin/main"), "{err}");
    // No claim was taken.
    let (_, out, _) = w.susi(&w.wt, &["tasks", "list"]);
    assert!(!out.contains("\"claimed_by\": \"TEST\""), "{out}");
    // After syncing, the same claim goes through.
    git(&w.wt, &["merge", "--quiet", "origin/main"]);
    let (code, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn claim_requires_a_synced_worktree_but_offline_is_not_an_error() {
    let w = World::new("claimoffline");
    add_task(&w);
    // Server gone: freshness is unknowable, and the claim then fails on the
    // remote itself, not on the sync check.
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    let (_, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert!(!err.contains("behind origin/main"), "{err}");
}
