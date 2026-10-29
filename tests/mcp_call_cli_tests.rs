#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Tests for direct `susi mcp call` CLI execution.

use std::process::Command;
use susi_paths::test_env::isolate_home;

fn run_susi(args: &[&str]) -> (i32, String, String) {
    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = std::env::temp_dir().join(format!(
        "susi-mcp-call-test-{}-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '-'], ""),
        id
    ));
    let home = scratch.join("home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let out = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .args(args)
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .unwrap();

    let _ = std::fs::remove_dir_all(&scratch);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn test_mcp_call_native_status_success() {
    let (code, stdout, stderr) = run_susi(&["mcp", "call", "susi", "status", "{}"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert_eq!(parsed["isError"], false);
    assert_eq!(parsed["content"][0]["type"], "text");
    assert!(parsed["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("SUSI Engine Version"));
}

#[test]
fn test_mcp_call_schema_validation_rejection() {
    // read_file requires "path"
    let (code, stdout, stderr) = run_susi(&["mcp", "call", "susi", "read_file", "{}"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert_eq!(parsed["isError"], true);
    assert!(parsed["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("inputSchema"));
}

#[test]
fn test_mcp_call_write_and_read_file_round_trip() {
    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = std::env::temp_dir().join(format!(
        "susi-mcp-file-roundtrip-{}-{}",
        std::process::id(),
        id
    ));
    let home = scratch.join("home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    // 1. write_file
    let out1 = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .args([
            "mcp",
            "call",
            "susi",
            "write_file",
            r#"{"path":"test.txt","content":"direct mcp execution works"}"#,
        ])
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .unwrap();

    assert_eq!(out1.status.code().unwrap_or(-1), 0);
    let parsed1: serde_json::Value =
        serde_json::from_slice(&out1.stdout).expect("write output must be valid JSON");
    assert_eq!(parsed1["isError"], false);

    // 2. read_file
    let out2 = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .args(["mcp", "call", "susi", "read_file", r#"{"path":"test.txt"}"#])
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .unwrap();

    assert_eq!(out2.status.code().unwrap_or(-1), 0);
    let parsed2: serde_json::Value =
        serde_json::from_slice(&out2.stdout).expect("read output must be valid JSON");
    assert_eq!(parsed2["isError"], false);
    assert_eq!(
        parsed2["content"][0]["text"].as_str().unwrap(),
        "direct mcp execution works"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
