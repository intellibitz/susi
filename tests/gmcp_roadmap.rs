#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Structural proof for the VC-203 GMCP completion program.
//!
//! Capability delivery is proved by the future requirement and mastery tests;
//! this test proves that the roadmap cannot silently omit a vector, requirement,
//! dependency, acceptance gate, terminal fan-in, or documented completion rule.

use std::path::Path;
use std::process::Command;

#[test]
fn gmcp_roadmap_is_structurally_complete() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shell = if cfg!(windows) { "sh" } else { "bash" };
    let output = Command::new(shell)
        .arg(root.join("scripts/check-gmcp-roadmap.sh"))
        .current_dir(root)
        .output()
        .expect("shell must run the GMCP roadmap checker");

    assert!(
        output.status.success(),
        "GMCP roadmap checker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("336 requirement tasks"),
        "checker did not verify the complete requirement queue"
    );
}

#[test]
fn unit_gmcp_roadmap_script_exists_and_is_non_empty() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/check-gmcp-roadmap.sh");
    let content = std::fs::read_to_string(&script).expect("read check-gmcp-roadmap.sh");
    assert!(content.contains("336"));
}

#[test]
fn unit_gmcp_roadmap_evidence_dir_contains_vc_203_artifacts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let evidence_dir = root.join(".agents/gmcp/evidence");
    assert!(evidence_dir.is_dir());
    let count = std::fs::read_dir(&evidence_dir)
        .expect("read evidence dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("VC-203-"))
        .count();
    assert!(count >= 1);
}

#[test]
fn unit_gmcp_roadmap_cargo_manifest_dir_resolves_workspace_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root.join("Cargo.toml").is_file());
    assert!(root.join("crates/susi-gmcp/Cargo.toml").is_file());
}

#[test]
fn unit_gmcp_contract_docs_directory_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(root.join(".agents/gmcp").is_dir());
    assert!(root.join("crates/susi-gmcp/src/lib.rs").is_file());
}
