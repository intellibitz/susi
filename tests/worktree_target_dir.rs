#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Every worktree used to keep a private `target/`, so a fresh worktree paid a
//! ~25 minute cold compile and held ~20 GB of artifacts identical to its
//! siblings'. `susi-worktree.sh` now links `target/` to the primary checkout's
//! — the parked tree's cache is the clone's shared one. These pin the link,
//! the ignore rule that keeps it out of commits, and the opt-out.

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
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_AGENT")
        .env_remove("SUSI_PRIVATE_TARGET");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-target-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("susi");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for s in ["susi-worktree.sh", "park-primary.sh", "setup-dev.sh"] {
            std::fs::copy(
                repo.join("scripts").join(s),
                primary.join("scripts").join(s),
            )
            .unwrap();
        }
        // The worktree's `target` link is ignored by the repo's .gitignore;
        // the fixture copies it so `git check-ignore` answers like the real
        // tree does.
        std::fs::copy(repo.join(".gitignore"), primary.join(".gitignore")).unwrap();
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        Self { root, primary }
    }

    fn worktree(&self, name: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
        let mut envs = envs.to_vec();
        envs.push(("SUSI_AGENT", "claude"));
        run(
            &self.primary,
            self.primary
                .join("scripts/susi-worktree.sh")
                .to_str()
                .unwrap(),
            &[name],
            &envs,
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cd_target(stdout: &str) -> PathBuf {
    let last = stdout.lines().last().unwrap();
    PathBuf::from(
        last.strip_prefix("cd ")
            .unwrap_or_else(|| panic!("last line must be `cd <path>`: {stdout}")),
    )
}

#[test]
fn a_new_worktree_shares_the_primarys_target_dir() {
    let w = World::new("shared");
    let (code, out, err) = w.worktree("worker", &[]);
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    let link = dest.join("target");
    assert!(
        link.symlink_metadata()
            .unwrap_or_else(|_| panic!("{link:?} must exist"))
            .file_type()
            .is_symlink(),
        "{link:?} must be a symlink, not a private dir"
    );
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(w.primary.join("target")).unwrap()
    );
    // A `git add -A` must never sweep the link in: `/target/` alone matches
    // only directories, so the rule had to cover the symlink itself.
    let (code, ignored, _) = run(&dest, "git", &["check-ignore", "target"], &[]);
    assert_eq!(code, 0, "worktree `target` link must be ignored: {ignored}");
    assert!(git(&dest, &["status", "--porcelain"]).is_empty());
}

#[test]
fn an_existing_private_target_is_left_alone() {
    let w = World::new("kept");
    let (code, out, err) = w.worktree("worker", &[]);
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    // The agent opted to keep its own build tree; re-provisioning the same
    // worktree must not silently convert it.
    std::fs::remove_file(dest.join("target")).unwrap();
    std::fs::create_dir(dest.join("target")).unwrap();
    let (code, _, err) = w.worktree("worker", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        !dest
            .join("target")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "a real target/ is the agent's; leave it"
    );
}

#[test]
fn susi_private_target_opts_out_of_sharing() {
    let w = World::new("private");
    let (code, out, err) = w.worktree("worker", &[("SUSI_PRIVATE_TARGET", "1")]);
    assert_eq!(code, 0, "{err}");
    assert!(!cd_target(&out).join("target").exists());
}
