// Mixed native/static and optional POSIX compatibility checks.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Release mechanics that run outside GitHub Actions: Windows promotion and
//! tagging a merged version bump. The release workflow files were removed
//! (CEO decision 2026-11-06).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
// Executes a legacy POSIX script/hook; native coverage remains enabled.
#[cfg(unix)]
fn windows_release_promotion() {
    // Run the real Windows process and file mechanics where they exist; the
    // same entrypoint checks shell syntax on other release builders.
    #[cfg(windows)]
    let bash = {
        let git = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
            .unwrap();
        PathBuf::from(String::from_utf8_lossy(&git.stdout).trim())
            .ancestors()
            .nth(3)
            .unwrap()
            .join("bin/bash.exe")
    };
    #[cfg(not(windows))]
    let bash = PathBuf::from("bash");
    let output = std::process::Command::new(bash)
        .arg("scripts/test-release-sync-windows.sh")
        .current_dir(root())
        .output()
        .expect("run release promotion regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn tag_merged_bump(work: &std::path::Path, sha: &str) -> std::process::Output {
    let script = root().join("scripts/release-tag-merged-bump.sh");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.env("MSYS_NO_PATHCONV", "1");
        c
    };
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(script)
        .arg(sha)
        .current_dir(work)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

/// The script the merge bot runs on every merge: an untagged version is tagged on
/// the merge commit and published; a version already tagged is left alone, so
/// the merges after a release build nothing.
#[test]
// Executes a legacy POSIX script/hook; native coverage remains enabled.
#[cfg(unix)]
fn a_merged_bump_is_tagged_once() {
    let root_dir =
        std::env::temp_dir().join(format!("susi-tag-merged-bump-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root_dir);
    let (bare, work) = (root_dir.join("server.git"), root_dir.join("work"));
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
    git(&work, &["init", "--quiet", "-b", "main"]);
    for key in ["commit.gpgsign", "tag.gpgsign"] {
        git(&work, &["config", key, "false"]);
    }
    git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    std::fs::write(
        work.join("Cargo.toml"),
        "[workspace]\nmembers = []\n\n[package]\nname = \"x\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
    git(&work, &["add", "Cargo.toml"]);
    git(&work, &["commit", "--quiet", "-m", "chore: release v9.9.9"]);
    git(&work, &["push", "--quiet", "origin", "HEAD:main"]);
    let bump = git(&work, &["rev-parse", "HEAD"]);

    let first = tag_merged_bump(&work, &bump);
    assert!(first.status.success(), "{first:?}");
    assert_eq!(
        String::from_utf8_lossy(&first.stdout).trim(),
        "v9.9.9",
        "the new tag is what the merge bot dispatches release.yml on"
    );
    let tagged = git(
        &work,
        &["ls-remote", "--tags", "origin", "refs/tags/v9.9.9^{}"],
    );
    assert!(
        tagged.starts_with(&bump),
        "v9.9.9 must be published on the bump: {tagged}"
    );

    // The next merge carries the same version: nothing to release.
    git(
        &work,
        &["commit", "--allow-empty", "--quiet", "-m", "later"],
    );
    git(&work, &["push", "--quiet", "origin", "HEAD:main"]);
    let later = git(&work, &["rev-parse", "HEAD"]);
    let second = tag_merged_bump(&work, &later);
    let still = git(
        &work,
        &["ls-remote", "--tags", "origin", "refs/tags/v9.9.9^{}"],
    );
    let _ = std::fs::remove_dir_all(&root_dir);
    assert!(second.status.success(), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already tagged")
            && second.stdout.is_empty(),
        "{second:?}"
    );
    assert!(
        still.starts_with(&bump),
        "an already-released version must not move: {still}"
    );
}
