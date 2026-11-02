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

#[test]
fn registered_stdio_server_requires_grants_for_both_discovery_and_call() {
    let scratch =
        std::env::temp_dir().join(format!("susi-registered-stdio-{}", std::process::id()));
    let home = scratch.join("home");
    let runtime = home.join(".susi");
    let marker = scratch.join("spawned");
    std::fs::create_dir_all(&runtime).unwrap();
    let source = scratch.join("peer.rs");
    let peer = scratch.join(format!("peer{}", std::env::consts::EXE_SUFFIX));
    // An actual Rust stdio process, with a marker written before it reads any
    // request. A rejected handshake cannot masquerade as a pre-spawn denial.
    std::fs::write(&source, r##"
use std::io::{self,BufRead,Write};
fn main() {
    std::fs::write(std::env::args_os().nth(1).unwrap(), "started").unwrap();
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let Some((_,rest))=line.split_once("\"id\"") else {continue};
        let id:String=rest.trim_start_matches(|c:char|c.is_whitespace()||c==':').chars().take_while(char::is_ascii_digit).collect();
        let result=if line.contains("tools/list") {
            r#"{"tools":[{"name":"check_env","inputSchema":{"type":"object"}}]}"#
        } else if line.contains("tools/call") {
            if std::env::var_os("SUSI_TEST_AMBIENT_SECRET").is_none() && std::env::var("PEER_SETTING").as_deref()==Ok("explicit") {
                r#"{"content":[{"type":"text","text":"isolated peer"}],"isError":false}"#
            } else { r#"{"content":[{"type":"text","text":"wrong environment"}],"isError":true}"# }
        } else if line.contains("initialize") {
            r#"{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"proof","version":"1"}}"#
        } else {continue};
        println!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}",id,result);
        io::stdout().flush().unwrap();
    }
}
"##).unwrap();
    let compiled = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition", "2021"])
        .arg(&source)
        .arg("-o")
        .arg(&peer)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let configure = |capabilities: &[&str]| {
        let config = serde_json::json!({"mcp_servers":{"proof":{"command":peer,"args":[marker],"env":{"PEER_SETTING":"explicit"},"capabilities":capabilities}}});
        std::fs::write(
            runtime.join("mcp_config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
    };
    let invoke = |tool: &str| {
        let out = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
            .env("SUSI_HOME", &runtime)
            .env("SUSI_TEST_AMBIENT_SECRET", "private-sentinel")
            .args(["mcp", "call", "proof", tool, "{}"])
            .current_dir(&scratch)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    configure(&["tools"]);
    std::fs::write(runtime.join("privacy_mode"), "local_only").unwrap();
    for tool in ["*", "check_env"] {
        let result = invoke(tool);
        assert_eq!(result["isError"], true, "{result}");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("process.exec"),
            "{result}"
        );
        assert!(!marker.exists());
    }
    // The same installed CLI can issue a scoped process grant; this permits
    // the peer without opening network egress or changing privacy mode.
    let grant = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .env("SUSI_HOME", &runtime)
        .args(["privacy", "grant", "--action", "process.exec", "--resource"])
        .arg(&peer)
        .current_dir(&scratch)
        .output()
        .unwrap();
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    for (capability, denied) in [
        ("network", "network.egress"),
        ("unknown-hardware", "unknown-hardware"),
    ] {
        configure(&[capability]);
        for tool in ["*", "check_env"] {
            let result = invoke(tool);
            assert_eq!(result["isError"], true, "{result}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains(denied),
                "{result}"
            );
            assert!(!marker.exists());
        }
    }
    configure(&["tools"]);
    let listing = invoke("*");
    assert_eq!(listing["isError"], false, "{listing}");
    assert!(
        listing["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("proof:check_env"),
        "{listing}"
    );
    let called = invoke("check_env");
    assert_eq!(called["isError"], false, "{called}");
    assert!(
        called["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("isolated peer"),
        "{called}"
    );
    assert!(marker.is_file());
    std::fs::remove_dir_all(scratch).unwrap();
}

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
