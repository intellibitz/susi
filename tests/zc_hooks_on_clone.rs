#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! A fresh clone gets the workflow hooks and ledger merge driver through
//! susi itself — no `setup-dev.sh`, no manual `git config`. The test
//! builds a bare fixture repo that carries `.githooks/` the way the real
//! repo does, then runs the susi binary inside it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `git config --get` exits 1 for an absent key — that absence is data,
/// not a failure, so this variant returns stdout whatever the status.
fn git_get(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn susi(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home/xdg"))
        .env_remove("SUSI_HOME")
        .env("GIT_TERMINAL_PROMPT", "0")
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

/// A "fresh clone" fixture: a git repo that ships `.githooks/` but whose
/// config has never been touched (exactly what clone leaves behind).
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("zc-hooks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".githooks")).unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    // Same hooks the real repo ships (content irrelevant to the mechanism).
    for h in ["commit-msg", "pre-commit", "pre-push", "workflow-guard"] {
        std::fs::write(root.join(".githooks").join(h), "#!/bin/sh\nexit 0\n").unwrap();
    }
    std::fs::write(root.join("scripts/merge-ledger.py"), "# ledger merger\n").unwrap();
    std::fs::write(
        root.join(".gitattributes"),
        ".agents/evidence.json merge=ledger\n",
    )
    .unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "init"]);
    root
}

#[test]
fn zc_hooks_on_clone_fresh_clone_has_no_hooks_config() {
    let root = fixture("clean");
    let hooks = git_get(&root, &["config", "--get", "core.hooksPath"]);
    // a clone does not run setup-dev.sh — hooks config must be absent here
    assert!(hooks.is_empty(), "fixture must start unconfigured: {hooks}");
    let driver = git_get(&root, &["config", "--get", "merge.ledger.driver"]);
    assert!(driver.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn zc_hooks_on_clone_susi_installs_hooks_and_merge_driver() {
    let root = fixture("install");
    // Any susi command inside the repo is the mechanism — workflow check is
    // the documented first command an agent runs.
    let (_code, _out) = susi(&root, &["workflow", "check", "--json"]);
    assert_eq!(
        git(&root, &["config", "--get", "core.hooksPath"]),
        ".githooks",
        "susi must set core.hooksPath without setup-dev.sh"
    );
    assert_eq!(
        git(&root, &["config", "--get", "merge.ledger.driver"]),
        "python3 scripts/merge-ledger.py %O %A %B"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn zc_hooks_on_clone_idempotent_and_hook_bits() {
    let root = fixture("idem");
    // strip exec bits to prove they get restored
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for h in ["commit-msg", "pre-commit", "pre-push", "workflow-guard"] {
            let p = root.join(".githooks").join(h);
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&p, perms).unwrap();
        }
    }
    let (_c1, _o1) = susi(&root, &["workflow", "check", "--json"]);
    let (_c2, _o2) = susi(&root, &["workflow", "check", "--json"]);
    assert_eq!(
        git(&root, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join(".githooks/pre-commit"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "hooks must be executable");
    }
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn zc_hooks_on_clone_ignores_repos_without_githooks() {
    let root = std::env::temp_dir().join(format!("zc-hooks-plain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "user.name", "t"]);
    std::fs::write(root.join("f.txt"), "x").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "init"]);
    // a repo that never shipped .githooks stays untouched
    let (_c, _o) = susi(&root, &["workflow", "check", "--json"]);
    assert!(git_get(&root, &["config", "--get", "core.hooksPath"]).is_empty());
    assert!(git_get(&root, &["config", "--get", "merge.ledger.driver"]).is_empty());
    std::fs::remove_dir_all(&root).ok();
}
