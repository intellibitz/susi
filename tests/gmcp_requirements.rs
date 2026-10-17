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
