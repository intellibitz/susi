#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn root() -> PathBuf {
    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "susi-audit-query-surface-{}-{serial}",
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

fn append_action(log: &Path, actor: &str, mission: &str, kind: &str, cost_micros: u64) {
    let details = serde_json::json!({
        "action_kind": kind,
        "actor": actor,
        "target": {"id": "redacted-target"},
        "outcome": "succeeded",
        "duration_ms": 17,
        "cost_micros": cost_micros,
        "correlation_id": mission,
        "redacted": true,
        "secret": "must-not-be-returned",
    })
    .to_string();
    susi_sandbox::audit_chain::append_signed_entry(log, "Info", "AUDIT_ACTION", &details, 7)
        .unwrap();
}

#[test]
fn audit_query_surface() {
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
    append_action(&log, "alice", "mission-a", "tool_execution", 17);
    append_action(&log, "bob", "mission-b", "model_call", 23);

    let (code, stdout, stderr) = run(
        &home,
        &workspace,
        &["audit", "query", "--json", "--actor", "alice"],
    );
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let records: Vec<Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["actor"], "alice");
    assert_eq!(records[0]["mission"], "mission-a");
    assert_eq!(records[0]["kind"], "tool_execution");
    assert_eq!(records[0]["cost_micros"], 17);
    assert!(!stdout.contains("must-not-be-returned"), "{stdout}");

    let (code, stdout, stderr) = run(
        &home,
        &workspace,
        &[
            "audit",
            "query",
            "--json",
            "--mission",
            "mission-b",
            "--kind",
            "model_call",
            "--since",
            "0",
        ],
    );
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let records: Vec<Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["cost_micros"], 23);

    let (code, stdout, stderr) = run(
        &home,
        &workspace,
        &["audit", "query", "--json", "--until", "0"],
    );
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let records: Vec<Value> = serde_json::from_str(&stdout).unwrap();
    assert!(records.is_empty(), "{stdout}");

    let mut lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut forged: Value = serde_json::from_str(&lines[0]).unwrap();
    forged["details"] = "mutated".into();
    lines[0] = serde_json::to_string(&forged).unwrap();
    std::fs::write(&log, format!("{}\n", lines.join("\n"))).unwrap();

    let (code, stdout, stderr) = run(&home, &workspace, &["audit", "query", "--json"]);
    assert_ne!(code, 0, "stdout={stdout}\nstderr={stderr}");
    assert!(
        stderr.contains("Audit query failed"),
        "stdout={stdout}\nstderr={stderr}"
    );

    drop(env);
    let _ = std::fs::remove_dir_all(root);
}
