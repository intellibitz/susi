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
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(workspace)
        .env("HOME", home)
        .env("USERPROFILE", home)
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
    susi_sandbox::audit_chain::append_signed_entry(&log, "Info", "FIRST", "one", 1).unwrap();
    susi_sandbox::audit_chain::append_signed_entry(&log, "Info", "SECOND", "two", 1).unwrap();

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
