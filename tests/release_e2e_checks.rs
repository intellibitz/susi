#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The built `susi` binary's end-to-end checks (Mandate 53, `E2E_CHECKS`):
//! what a release of open core must do on its own newest behaviour, run here
//! against the built binary on every test run, each in a scratch home of its
//! own, so a check that no longer matches reality fails at once, not first at
//! release time.
//!
//! The release gate that also runs them (`admin release`) is Susi Commercial
//! Apps' dev workflow since the lean-core move, together with the checks of
//! the commands that moved with it (the task queue, `workflow check`, the
//! release cut's refusals), which run against `susi-commercial` there.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use susi_paths::test_env::{isolate_home, EnvGuard};
use susi_paths::SusiDirs;

/// One end-to-end check of the built binary.
struct E2eCheck {
    name: &'static str,
    args: &'static [&'static str],
    exit: i32,
    stdout_has: &'static [&'static str],
    stderr_has: &'static [&'static str],
    /// The command must leave its working directory empty.
    leaves_no_files: bool,
}

/// What the built binary must do on its own newest behaviour.
const E2E_CHECKS: &[E2eCheck] = &[
    E2eCheck {
        name: "first-run setup explains readiness without configuration",
        args: &["setup"],
        exit: 0,
        stdout_has: &["give me a goal", "Cloud:", "Local AI:"],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "brain classifies a coding prompt",
        args: &[
            "brain", "classify", "fix", "this", "bug", "in", "my", "parser",
        ],
        exit: 0,
        stdout_has: &["\"task_class\": \"code\""],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "brain status reports ranking, budget and routing state",
        args: &["brain", "status", "--json"],
        exit: 0,
        stdout_has: &[
            "ranking_by_task_class",
            "\"budget\"",
            "cooled_providers",
            "failure_streaks",
        ],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "ecosystem scan reports engines and accelerators",
        args: &["ecosystem", "scan"],
        exit: 0,
        stdout_has: &["\"engines\"", "\"accelerators\"", "\"hardware\""],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "a flag-shaped typo is refused, not run as a mission",
        args: &["release", "--help"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["looks like a command", "susi-commercial admin release"],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "a nested command word is refused with a path hint",
        args: &["scan"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["susi ecosystem scan"],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "the moved task queue refuses naming its commercial command",
        args: &["tasks", "claim", "T-A-1", "--scope", "src"],
        exit: 1,
        stdout_has: &[],
        stderr_has: &["susi-commercial tasks claim"],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "the moved workflow refuses naming its commercial command",
        args: &["workflow", "check"],
        exit: 1,
        stdout_has: &[],
        stderr_has: &["susi-commercial workflow check"],
        leaves_no_files: true,
    },
];

/// One test swaps this process's profile for a decoy, and every check's
/// child inherits this process's env: one test at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// One check's run: `args` in `cwd`, under `home` as its whole home, with git
/// unable to walk out of `ceiling` (a surrounding repository must not be
/// discovered) and no inherited instance root or port offset.
fn e2e_command(args: &[&str], home: &Path, cwd: &Path, ceiling: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut cmd, home);
    cmd.args(args)
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_PORT_OFFSET");
    cmd
}

fn verdict(
    check: &E2eCheck,
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
    files: &[String],
) -> Result<(), String> {
    if exit != Some(check.exit) {
        return Err(format!("exit {exit:?}, expected {}", check.exit));
    }
    if let Some(missing) = check.stdout_has.iter().find(|n| !stdout.contains(*n)) {
        return Err(format!("stdout lacks `{missing}`"));
    }
    if let Some(missing) = check.stderr_has.iter().find(|n| !stderr.contains(*n)) {
        return Err(format!("stderr lacks `{missing}`"));
    }
    if check.leaves_no_files && !files.is_empty() {
        return Err(format!("left files behind: {files:?}"));
    }
    Ok(())
}

/// Run every check in its own `check-<i>` scratch dir under `scratch`, in
/// parallel; the names of the passing checks, or the first failure.
fn run_e2e_checks(scratch: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    let ceiling = scratch.canonicalize().map_err(|e| e.to_string())?;
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = E2E_CHECKS
            .iter()
            .enumerate()
            .map(|(i, check)| {
                let ceiling = &ceiling;
                scope.spawn(move || -> Result<String, String> {
                    let root = scratch.join(format!("check-{i}"));
                    let (home, cwd) = (root.join("home"), root.join("cwd"));
                    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
                    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
                    let out = e2e_command(check.args, &home, &cwd, ceiling)
                        .output()
                        .map_err(|e| e.to_string())?;
                    let files: Vec<String> = std::fs::read_dir(&cwd)
                        .map_err(|e| e.to_string())?
                        .filter_map(Result::ok)
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    verdict(
                        check,
                        out.status.code(),
                        &String::from_utf8_lossy(&out.stdout),
                        &String::from_utf8_lossy(&out.stderr),
                        &files,
                    )
                    .map_err(|why| {
                        format!(
                            "e2e check `{}` (susi {}) failed: {why}",
                            check.name,
                            check.args.join(" ")
                        )
                    })?;
                    Ok(check.name.to_string())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("e2e check panicked".to_string()))
            })
            .collect::<Vec<_>>()
    });
    results.into_iter().collect()
}

#[test]
fn every_release_e2e_check_passes_against_the_built_binary() {
    let _lock = env_lock();
    let scratch = std::env::temp_dir().join(format!("susi-e2e-checks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let passed = run_e2e_checks(&scratch).unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(passed.len(), E2E_CHECKS.len());
}

/// A scratch dir under `target/` sits inside the repository being released.
/// A check must never discover that surrounding repository.
#[test]
fn e2e_checks_do_not_discover_a_surrounding_repo() {
    let _lock = env_lock();
    let outer = std::env::temp_dir().join(format!("susi-e2e-surrounding-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outer);
    std::fs::create_dir_all(outer.join(".agents/tasks")).unwrap();
    let out = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&outer)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init");
    std::fs::write(
        outer.join(".agents/tasks/T-OUTER-1.json"),
        r#"{"id":"T-OUTER-1","title":"outer","accept":{"cmd":["cargo","--version"]},"created_by":"OUTER","created_unix":0}"#,
    )
    .unwrap();
    let scratch = outer.join("target").join("release-e2e-scratch");
    let passed = run_e2e_checks(&scratch).unwrap_or_else(|e| panic!("{e}"));
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

/// A check run from a developer's shell must not reach that developer's
/// profile: with only `HOME` / `USERPROFILE` moved, a Windows check took the
/// release instance's `cloud.env` (provider keys) and models from the shell's
/// `%APPDATA%` / `%LOCALAPPDATA%` and linked them into its scratch home.
#[test]
fn e2e_checks_do_not_reach_the_launching_profile() {
    let _lock = env_lock();
    let root = std::env::temp_dir().join(format!("susi-e2e-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (decoy, scratch) = (root.join("decoy"), root.join("scratch"));
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

    let passed = run_e2e_checks(&scratch);

    drop(env);
    let after = tree(&decoy);
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

#[test]
fn unit_e2e_checks_table_has_unique_non_empty_names_and_args() {
    let mut names = std::collections::BTreeSet::new();
    for check in E2E_CHECKS {
        assert!(!check.name.is_empty());
        assert!(!check.args.is_empty());
        assert!(
            !check.stdout_has.is_empty() || !check.stderr_has.is_empty(),
            "{} must assert on output, not only the exit code",
            check.name
        );
        assert!(
            names.insert(check.name),
            "duplicate E2E_CHECKS name: {}",
            check.name
        );
    }
}
