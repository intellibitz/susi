#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/check-release-tag.sh`: a release tag must be the workspace version
//! and must be on `main`.
//!
//! The release workflow publishes binaries on any `v*.*.*` tag, and until this
//! script existed nothing checked that the tag named the version in `Cargo.toml`
//! or that `main` contained the tagged commit — so a tag on a branch could
//! publish code no one merged, under a version no one cut.

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

/// A clone with `main` pushed to a bare origin and a `Cargo.toml` that says
/// what the test needs it to say.
struct World {
    root: PathBuf,
    repo: PathBuf,
}

impl World {
    fn new(tag: &str, version: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-reltag-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["config", "user.name", "test"]);
        git(&repo, &["config", "user.email", "test@example.test"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(
            repo.join("Cargo.toml"),
            format!("[package]\nname = \"susi\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "--quiet", "origin"]);
        Self { root, repo }
    }

    /// A branch off main that main does not contain.
    fn side_branch(&self) {
        git(&self.repo, &["switch", "--quiet", "-c", "side"]);
        std::fs::write(self.repo.join("feature.txt"), "unmerged").unwrap();
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", "side work"]);
    }

    fn check(&self, tag: &str) -> (i32, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-release-tag.sh");
        let out = Command::new(script)
            .arg(tag)
            .current_dir(&self.repo)
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
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_tag_that_is_the_workspace_version_on_main_passes() {
    let w = World::new("ok", "1.2.3");
    git(&w.repo, &["tag", "v1.2.3"]);
    let (code, out) = w.check("v1.2.3");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("is v1.2.3 on origin/main"), "{out}");
}

#[test]
fn a_tag_that_contradicts_the_workspace_version_is_refused() {
    let w = World::new("version", "1.2.3");
    git(&w.repo, &["tag", "v1.2.4"]);
    let (code, out) = w.check("v1.2.4");
    assert_eq!(code, 1);
    assert!(
        out.contains("is not the workspace version") && out.contains("v1.2.3"),
        "the message must say what the version is: {out}"
    );
}

#[test]
fn a_tag_on_a_branch_main_does_not_contain_is_refused() {
    let w = World::new("off-main", "1.2.3");
    w.side_branch();
    git(&w.repo, &["tag", "v1.2.3"]);
    let (code, out) = w.check("v1.2.3");
    assert_eq!(code, 1);
    assert!(
        out.contains("is not an ancestor of origin/main"),
        "a release must be code main contains: {out}"
    );
    // The same version, tagged on main instead, is accepted: it is the commit
    // that matters, not the tag's name.
    git(&w.repo, &["switch", "--quiet", "main"]);
    git(&w.repo, &["tag", "--force", "v1.2.3"]);
    let (code, out) = w.check("v1.2.3");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_missing_tag_is_refused() {
    let w = World::new("missing", "1.2.3");
    let (code, out) = w.check("v1.2.3");
    assert_eq!(code, 1);
    assert!(out.contains("no such tag"), "{out}");
}
