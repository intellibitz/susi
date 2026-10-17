#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Acceptance surface for the VC-203 GMCP completion program.
//!
//! Every queued `T-CODEXGMCPROADMAP-*` task is gated on a `gmcp_req_203_*`
//! test living in this file; this first contract test proves the normative
//! requirements ledger (VC-203-001) is pinned, verbatim, fully classified,
//! and fail-closed against upstream drift.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn gmcp_req_203_001_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("python3")
        .arg(root.join("scripts/check-gmcp-ledger.py"))
        .current_dir(root)
        .output()
        .expect("python3 must run the GMCP ledger checker");

    assert!(
        output.status.success(),
        "GMCP ledger checker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("requirements rows"),
        "checker did not verify the complete classified ledger"
    );
}

/// GMCP-203-001-02: the shipped binary verifies the pinned ledger through
/// `susi_gmcp::requirements_ledger` — the same production path every GMCP
/// entry point links — not the authoring-time Python checker.
#[test]
fn gmcp_req_203_001_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-ledger-test-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["mcp", "ledger"])
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .output()
        .expect("the susi binary must run `mcp ledger`");
    let _ = std::fs::remove_dir_all(&scratch);

    assert!(
        output.status.success(),
        "susi mcp ledger failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("`mcp ledger` must print a JSON report");
    assert_eq!(report["verified"], true);
    assert!(report["rows"].as_u64().unwrap_or(0) > 0);
    assert!(report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0);
    assert!(report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0);
}

fn susi_command(root: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    command
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME");
    command
}

fn susi_json(root: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let output = susi_command(root, home, args)
        .output()
        .unwrap_or_else(|e| panic!("the susi binary must run {args:?}: {e}"));
    assert!(
        output.status.success(),
        "susi {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .unwrap_or_else(|e| panic!("susi {args:?} must print JSON: {e}"))
}

fn call_tool_report(result: &serde_json::Value) -> serde_json::Value {
    assert_eq!(
        result["isError"], false,
        "tool call returned an error: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("CallToolResult must carry a text block");
    serde_json::from_str(text).expect("ledger_verify must emit the canonical JSON report")
}

/// GMCP-203-001-03: the ledger verification capability is wired through the
/// CLI (`susi mcp ledger`), the direct-call surface (`susi mcp call susi`,
/// the same path daemon-embedded dispatch uses), and the served MCP surface
/// (`susi mcp serve` over stdio JSON-RPC — the gateway/server entry point).
/// Every entry point dispatches to `requirements_ledger::verify_report_json`,
/// so all three must return the identical canonical report, and `tools/list`
/// must advertise `ledger_verify` through the shared registry.
#[test]
fn gmcp_req_203_001_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-ledger-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // Entry point 1: the CLI control surface.
    let cli_report = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(cli_report["verified"], true);

    // Entry point 2: the direct-call surface — both native aliases must
    // resolve to the same handler.
    for server in ["susi", "native"] {
        let call = susi_json(root, &home, &["mcp", "call", server, "ledger_verify", "{}"]);
        let report = call_tool_report(&call);
        assert_eq!(
            report, cli_report,
            "{server}:ledger_verify must return the canonical report"
        );
    }

    // Entry point 3: the served MCP surface — a stdio JSON-RPC session that
    // dispatches through the shared plane-bus tool registry, the same path
    // the daemon and gateway use.
    let mut child = susi_command(root, &home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the susi binary must spawn `mcp serve`");
    let mut stdin = child.stdin.take().expect("stdio server must accept stdin");
    let stdout = child.stdout.take().expect("stdio server must yield stdout");
    let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gmcp-it","version":"1.0"}}}"#
                .as_slice(),
        )
        .and_then(|()| {
            stdin.write_all(b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        })
        .expect("handshake must write");
    stdin.flush().expect("requests must flush");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut next_id = 2u64;
    let mut request = |params: serde_json::Value| {
        let id = next_id;
        next_id += 1;
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": params["method"], "params": params["params"]})
        )
        .expect("request must write");
        stdin.flush().expect("request must flush");
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                remaining > std::time::Duration::ZERO,
                "stdio server timed out"
            );
            let value = rx
                .recv_timeout(remaining)
                .expect("stdio server must answer");
            if value["id"].as_u64() == Some(id) {
                assert!(
                    value.get("error").is_none_or(|e| e.is_null()),
                    "JSON-RPC error: {value}"
                );
                return value["result"].clone();
            }
        }
    };

    // tools/list must advertise ledger_verify through the shared registry.
    let mut cursor = serde_json::Value::Null;
    let mut listed = false;
    for _ in 0..16 {
        let result = request(serde_json::json!({
            "method": "tools/list",
            "params": (if cursor.is_null() {
                serde_json::json!({})
            } else {
                serde_json::json!({"cursor": cursor})
            })
        }));
        if result["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"] == "ledger_verify"))
        {
            listed = true;
            break;
        }
        match result["nextCursor"]
            .as_str()
            .or_else(|| result["next_cursor"].as_str())
        {
            Some(next) => cursor = serde_json::json!(next),
            None => break,
        }
    }
    assert!(listed, "tools/list must advertise ledger_verify");

    // tools/call over the wire must return the canonical report.
    let result = request(serde_json::json!({
        "method": "tools/call",
        "params": {"name": "ledger_verify", "arguments": {}}
    }));
    let wire_report = call_tool_report(&result);
    assert_eq!(
        wire_report, cli_report,
        "served tools/call must return the canonical report"
    );

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&scratch);
}
