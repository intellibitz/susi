#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Tests for unified operator control surface CLI (`susi top`, VC-201-099).

use std::fs;
use std::process::Command;
use susi_gemi::tui_top::UnifiedOperatorState;
use susi_paths::test_env::isolate_home;

fn run_susi_in_dir(args: &[&str], custom_cwd: Option<&std::path::Path>) -> (i32, String, String) {
    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = std::env::temp_dir().join(format!(
        "susi-top-test-{}-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '-'], ""),
        id
    ));
    let home = scratch.join("home");
    fs::create_dir_all(&home).unwrap();

    let cwd = match custom_cwd {
        Some(p) => p.to_path_buf(),
        None => {
            let default_cwd = scratch.join("cwd");
            fs::create_dir_all(&default_cwd).unwrap();
            default_cwd
        }
    };

    let out = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .args(args)
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .unwrap();

    let _ = fs::remove_dir_all(&scratch);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_susi(args: &[&str]) -> (i32, String, String) {
    run_susi_in_dir(args, None)
}

#[test]
fn test_top_renders_unified_control_surface_dimensions() {
    let (code, stdout, stderr) = run_susi(&["top"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("susi top — unified operator control surface"));
    assert!(stdout.contains("── workload ──"));
    assert!(stdout.contains("── resources ──"));
    assert!(stdout.contains("── providers ──"));
    assert!(stdout.contains("── configuration ──"));
    assert!(stdout.contains("── federation ──"));
    assert!(stdout.contains("── evidence ──"));
}

#[test]
fn test_top_json_emits_valid_unified_state() {
    let (code, stdout, stderr) = run_susi(&["top", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: UnifiedOperatorState =
        serde_json::from_str(&stdout).expect("stdout must be valid UnifiedOperatorState JSON");
    assert!(parsed.resources.is_some());
    assert!(parsed.config.is_some());
    assert!(parsed.federation.is_some());
    assert!(parsed.evidence.is_some());
    assert!(!parsed.snapshot.providers.is_empty());
}

#[test]
fn test_top_filter_dimensions() {
    let (code, stdout, stderr) = run_susi(&["top", "--filter", "workload"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── workload ──"));
    assert!(!stdout.contains("── providers ──"));
    assert!(!stdout.contains("── resources ──"));

    let (code, stdout, stderr) = run_susi(&["top", "--filter", "resources"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── resources ──"));
    assert!(!stdout.contains("── workload ──"));

    let (code, stdout, stderr) = run_susi(&["top", "--filter", "providers"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── providers ──"));
    assert!(!stdout.contains("── workload ──"));

    let (code, stdout, stderr) = run_susi(&["top", "--filter", "config"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── configuration ──"));

    let (code, stdout, stderr) = run_susi(&["top", "--filter", "federation"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── federation ──"));

    let (code, stdout, stderr) = run_susi(&["top", "--filter", "evidence"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("── evidence ──"));
}

#[test]
fn test_top_drill_down_explains_blocked_work() {
    let temp = std::env::temp_dir().join(format!(
        "susi-top-blocked-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let tasks_dir = temp.join(".agents").join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    let task_b = r#"{
        "id": "T-TEST-DEP-02",
        "title": "Prerequisite task",
        "claimed_by": "AGENT1",
        "deps": []
    }"#;
    let task_a = r#"{
        "id": "T-TEST-BLOCKED-01",
        "title": "Blocked task",
        "deps": ["T-TEST-DEP-02"]
    }"#;

    fs::write(tasks_dir.join("T-TEST-DEP-02.json"), task_b).unwrap();
    fs::write(tasks_dir.join("T-TEST-BLOCKED-01.json"), task_a).unwrap();

    let (code, stdout, stderr) = run_susi_in_dir(&["top"], Some(&temp));
    assert_eq!(code, 0, "stderr: {stderr}");

    assert!(stdout.contains("T-TEST-BLOCKED-01 — Blocked task (unmet dependency: T-TEST-DEP-02)"));

    let _ = fs::remove_dir_all(&temp);
}
