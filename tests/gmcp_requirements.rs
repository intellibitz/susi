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

use std::path::Path;
use std::process::Command;

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
