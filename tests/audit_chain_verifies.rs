#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use susi_paths::test_env::isolate_home;

fn root() -> PathBuf {
    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "susi-audit-chain-verifies-{}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run(home: &Path, workspace: &Path, args: &[&str]) -> (i32, String, String) {
    let output = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), home)
        .args(args)
        .current_dir(workspace)
        .env("SUSI_HOME", home.join("instance"))
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("SUSI_PORT_OFFSET")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn audit_chain_verifies() {
    let _guard = UNIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(workspace.join(".susi")).unwrap();

    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &home)
        .set("USERPROFILE", &home)
        .set("SUSI_HOME", home.join("instance"))
        .set("XDG_CONFIG_HOME", home.join("xdg"))
        .set("XDG_DATA_HOME", home.join("xdg-data"));

    let log = workspace.join(".susi/audit.log");
    susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "FIRST", "one", 1).unwrap();
    susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "SECOND", "two", 1)
        .unwrap();

    let (code, stdout, stderr) = run(&home, &workspace, &["audit", "verify"]);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("Audit chain verified: 2 record(s)"),
        "{stdout}"
    );

    let (code, stdout, stderr) = run(&home, &workspace, &["audit", "verify", "--since", "0"]);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("2 record(s) at or after Unix timestamp 0"),
        "{stdout}"
    );

    let mut lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut forged: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    forged["details"] = "mutated".into();
    lines[0] = serde_json::to_string(&forged).unwrap();
    std::fs::write(&log, format!("{}\n", lines.join("\n"))).unwrap();

    let (code, stdout, stderr) = run(&home, &workspace, &["audit", "verify"]);
    assert_ne!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("Audit chain verification failed"),
        "stdout={stdout}\nstderr={stderr}"
    );

    drop(env);
    let _ = std::fs::remove_dir_all(root);
}

static UNIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn unit_audit_chain_verify_empty_log_returns_zero_and_missing_errors() {
    let _guard = UNIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = root();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", dir.join("instance"));
    let missing = dir.join("nonexistent-audit.log");
    assert!(susi_sandbox_client::audit_chain::verify_chain(&missing).is_err());
    let empty = dir.join("empty-audit.log");
    std::fs::write(&empty, "").unwrap();
    assert_eq!(
        susi_sandbox_client::audit_chain::verify_chain(&empty).unwrap(),
        0
    );
    drop(env);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unit_audit_chain_append_links_genesis_and_subsequent_hashes() {
    let _guard = UNIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = root();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", dir.join("instance"));
    let log = dir.join("audit.log");
    let h1 =
        susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "BOOT", "init", 10)
            .unwrap();
    let h2 =
        susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "STEP", "next", 10)
            .unwrap();
    assert_eq!(h1.len(), 64);
    assert_eq!(h2.len(), 64);
    assert_ne!(h1, h2);
    assert_eq!(
        susi_sandbox_client::audit_chain::verify_chain(&log).unwrap(),
        2
    );
    drop(env);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unit_audit_chain_verify_detects_tampered_prev_hash() {
    let _guard = UNIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = root();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", dir.join("instance"));
    let log = dir.join("audit.log");
    susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "A", "1", 1).unwrap();
    susi_sandbox_client::audit_chain::append_signed_entry(&log, "Info", "B", "2", 1).unwrap();

    let mut lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut second: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    second["prev_hash"] = "0".repeat(64).into();
    lines[1] = serde_json::to_string(&second).unwrap();
    std::fs::write(&log, format!("{}\n", lines.join("\n"))).unwrap();

    assert!(susi_sandbox_client::audit_chain::verify_chain(&log).is_err());
    drop(env);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unit_root_helper_creates_distinct_clean_directories() {
    let d1 = root();
    let d2 = root();
    assert_ne!(d1, d2);
    assert!(d1.is_dir());
    assert!(d2.is_dir());
    let _ = std::fs::remove_dir_all(d1);
    let _ = std::fs::remove_dir_all(d2);
}
