#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The release gate's end-to-end checks (`susi_gawd::admin::E2E_CHECKS`) run
//! here against the built binary on every test run, so a check that no longer
//! matches reality fails in CI — not first at release time.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use susi_gawd::admin::{SusiAdmin, E2E_CHECKS};
use susi_paths::test_env::EnvGuard;
use susi_paths::SusiDirs;

/// One test swaps this process's profile for a decoy, and every check's
/// child inherits this process's env: one test at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn every_release_e2e_check_passes_against_the_built_binary() {
    let _lock = env_lock();
    let scratch = std::env::temp_dir().join(format!("susi-e2e-checks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let passed =
        SusiAdmin::run_e2e_checks(std::path::Path::new(env!("CARGO_BIN_EXE_susi")), &scratch)
            .unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(passed.len(), E2E_CHECKS.len());
}

/// The gate's scratch dir sits under `target/` inside the repo being released.
/// A check must never discover that surrounding repo (the v0.18.0 cut aborted
/// when `susi tasks` listed the real queue instead of an empty one).
#[test]
fn e2e_checks_do_not_discover_a_surrounding_repo() {
    let _lock = env_lock();
    let outer = std::env::temp_dir().join(format!("susi-e2e-surrounding-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outer);
    std::fs::create_dir_all(outer.join(".agents/tasks")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&outer)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "--quiet"]);
    // A task the checks must NOT see.
    std::fs::write(
        outer.join(".agents/tasks/T-OUTER-1.json"),
        r#"{"id":"T-OUTER-1","title":"outer","accept":{"cmd":["cargo","--version"]},"created_by":"OUTER","created_unix":0}"#,
    )
    .unwrap();
    let scratch = outer.join("target").join("release-e2e-scratch");
    let passed =
        SusiAdmin::run_e2e_checks(std::path::Path::new(env!("CARGO_BIN_EXE_susi")), &scratch)
            .unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&outer);
    assert_eq!(passed.len(), E2E_CHECKS.len());
}

/// Every file and dir under `dir`, relative to it.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path.clone());
            }
            out.push(path.strip_prefix(dir).unwrap().to_path_buf());
        }
    }
    out.sort();
    out
}

/// A release cut from a developer's shell must not reach that developer's
/// profile. The binary under test is a dev build: it shares the release
/// instance's `cloud.env` (provider keys) and models with its scratch
/// instance by symlink, resolving them from the env it was handed. With only
/// `HOME` / `USERPROFILE` moved, a Windows check took them from the shell's
/// `%APPDATA%` / `%LOCALAPPDATA%` (a Linux one its models from
/// `XDG_DATA_HOME`) and linked the operator's real keys into every check's
/// scratch home.
#[test]
fn e2e_checks_do_not_reach_the_launching_profile() {
    let _lock = env_lock();
    let root = std::env::temp_dir().join(format!("susi-e2e-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (decoy, scratch) = (root.join("decoy"), root.join("scratch"));

    // This process stands in for the developer's shell: its whole profile
    // (`EnvGuard` moves `USERPROFILE` and the app-data dirs with `HOME`) is
    // the decoy, holding a release instance with keys and models.
    let mut env = EnvGuard::isolated();
    env.set("HOME", &decoy)
        .set("XDG_CONFIG_HOME", decoy.join(".config"))
        .set("XDG_DATA_HOME", decoy.join(".local").join("share"))
        .set("XDG_CACHE_HOME", decoy.join(".cache"))
        .remove("SUSI_XDG");
    let (config, data) = (SusiDirs::config_dir(), SusiDirs::data_dir());
    assert!(
        config.starts_with(&decoy) && data.starts_with(&decoy),
        "the decoy release instance must sit in the decoy profile"
    );
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("cloud.env"), "DECOY_PROVIDER_KEY=decoy\n").unwrap();
    std::fs::create_dir_all(data.join("models")).unwrap();
    let before = tree(&decoy);

    let passed = SusiAdmin::run_e2e_checks(Path::new(env!("CARGO_BIN_EXE_susi")), &scratch);

    drop(env);
    let after = tree(&decoy);
    // A check may make its own models dir; one that is a link, or that lands
    // in the decoy, is the launching profile's.
    let real_decoy = decoy.canonicalize().unwrap();
    let linked: Vec<PathBuf> = (0..E2E_CHECKS.len())
        .flat_map(|i| {
            let dev = scratch
                .join(format!("check-{i}"))
                .join("home")
                .join(".susi-dev");
            [dev.join("cloud.env"), dev.join("models")]
        })
        .filter(|shared| {
            shared
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
                || shared
                    .canonicalize()
                    .is_ok_and(|real| real.starts_with(&real_decoy))
        })
        .collect();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        passed.unwrap_or_else(|e| panic!("{e}")).len(),
        E2E_CHECKS.len()
    );
    assert!(
        linked.is_empty(),
        "a check linked the launching profile's release instance into its \
         scratch home: {linked:?}"
    );
    assert_eq!(after, before, "a check wrote into the launching profile");
}

/// `release.yml`'s Windows leg re-runs these checks by hand in PowerShell
/// (`Check '<name>' <exit> '<needle>' @('<arg>', ...)`). Each line must be an
/// `E2E_CHECKS` entry: v0.21.24 shipped no Windows binary because that leg
/// still ran bare `susi brain` (a table for people) after the check moved to
/// `brain status --json`.
#[test]
fn windows_release_checks_match_e2e_checks() {
    let yml = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .unwrap();
    let lines: Vec<&str> = yml
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("Check '"))
        .collect();
    assert!(!lines.is_empty(), "release.yml runs no Windows e2e checks");
    for line in lines {
        // Split on single quotes: quoted fields sit at odd indices.
        let parts: Vec<&str> = line.split('\'').collect();
        let quoted: Vec<&str> = parts.iter().skip(1).step_by(2).copied().collect();
        let exit: i32 = parts[2].trim().parse().unwrap();
        let (needle, args) = (quoted[1], &quoted[2..]);
        let check = E2E_CHECKS
            .iter()
            .find(|c| c.args == args)
            .unwrap_or_else(|| {
                panic!(
                    "release.yml runs `susi {}`, which no E2E_CHECKS entry runs",
                    args.join(" ")
                )
            });
        assert_eq!(check.exit, exit, "{line}");
        assert!(
            check.stdout_has.contains(&needle) || check.stderr_has.contains(&needle),
            "release.yml expects {needle:?}, which E2E_CHECKS \"{}\" does not: {line}",
            check.name
        );
    }
}
