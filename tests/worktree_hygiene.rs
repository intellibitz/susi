#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Worktree hygiene, through the real scripts.
//!
//! `scripts/prune-worktrees.sh` is only allowed to reclaim a worktree whose
//! work cannot be lost — clean, and already contained in `origin/main` — and it
//! must never remove another agent's. `scripts/susi-worktree.sh` must refuse to
//! reuse a directory that belongs to a different agent, because two workers in
//! one tree share one token and one branch.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
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
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let (code, out) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {out}");
}

fn script(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

struct World {
    root: PathBuf,
    repo: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-hygiene-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let repo = root.join("repo");
        for dir in [&bare, &repo] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        // Per-worktree config is what carries `susi.agent` in the real flow.
        git(&repo, &["config", "extensions.worktreeConfig", "true"]);
        git(&repo, &["commit", "--allow-empty", "--quiet", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "--quiet", "origin"]);
        Self { root, repo }
    }

    /// A linked worktree with its own branch, owner, and optional changes.
    fn worktree(&self, name: &str, owner: &str, kind: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                name,
                path.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(&path, &["config", "--worktree", "susi.agent", owner]);
        match kind {
            "finished" => {}
            "dirty" => std::fs::write(path.join("scratch.txt"), "uncommitted").unwrap(),
            "unmerged" => {
                std::fs::write(path.join("work.txt"), "not merged").unwrap();
                git(&path, &["add", "work.txt"]);
                git(&path, &["commit", "--quiet", "-m", "unmerged work"]);
            }
            other => panic!("unknown worktree kind {other}"),
        }
        path
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn prune_reclaims_only_work_that_cannot_be_lost() {
    let w = World::new("prune");
    // The runner holds the identity the script compares owners against.
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    let dirty = w.worktree("dirty", "TEST", "dirty");
    let unmerged = w.worktree("unmerged", "TEST", "unmerged");
    let other = w.worktree("other", "OTHER", "finished");
    let prune = script("prune-worktrees.sh");

    // Report first: the default must not touch anything.
    let (code, out) = run(&runner, &prune, &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("reclaimable"), "{out}");
    assert!(out.contains("uncommitted changes"), "{out}");
    assert!(out.contains("unmerged commits"), "{out}");
    assert!(out.contains("another agent"), "{out}");
    for path in [&finished, &dirty, &unmerged, &other] {
        assert!(path.exists(), "the dry run removed {path:?}");
    }

    // --apply reclaims this agent's finished worktree, and only that.
    let (code, out) = run(&runner, &prune, &["--apply"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !finished.exists(),
        "a merged, clean worktree is reclaimable"
    );
    assert!(dirty.exists(), "uncommitted work must be kept: {out}");
    assert!(unmerged.exists(), "unmerged commits must be kept: {out}");
    assert!(
        other.exists(),
        "another agent's worktree must be kept: {out}"
    );
    assert!(runner.exists(), "the worktree this ran from must be kept");

    // --all is the deliberate override for a shared clone.
    let (code, out) = run(&runner, &prune, &["--apply", "--all"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !other.exists(),
        "--all reclaims another agent's finished tree"
    );
    assert!(dirty.exists() && unmerged.exists() && runner.exists());
}

#[test]
fn prune_refuses_to_guess_when_origin_is_unreachable() {
    let w = World::new("offline");
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    let (code, out) = run(&runner, &script("prune-worktrees.sh"), &["--apply"], &[]);
    assert_ne!(code, 0, "an unreachable origin must refuse: {out}");
    assert!(out.contains("cannot reach origin"), "{out}");
    assert!(finished.exists(), "nothing may be removed while unsure");
}

/// Two workers in one directory share one token and one branch, so the reuse
/// path must refuse a directory that belongs to a different agent.
#[test]
fn worktree_reuse_refuses_another_agents_directory() {
    let w = World::new("reuse");
    // The script derives the destination from the primary's name and the
    // worktree name, exactly as the fixture lays it out.
    let name = "taken";
    let path = w.root.join(format!(
        "{}-{name}",
        w.repo.file_name().unwrap().to_str().unwrap()
    ));
    git(
        &w.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            path.to_str().unwrap(),
            "origin/main",
        ],
    );
    git(&path, &["config", "--worktree", "susi.agent", "OTHERAGENT"]);

    let (code, out) = run(
        &w.repo,
        &script("susi-worktree.sh"),
        &[name],
        &[("SUSI_PRIMARY_WATCH_DISABLE", "1"), ("SUSI_AGENT", "test")],
    );
    assert_ne!(
        code, 0,
        "reusing another agent's directory must refuse: {out}"
    );
    assert!(out.contains("already belongs to agent OTHERAGENT"), "{out}");
    assert!(out.contains("One worktree per worker"), "{out}");
}
