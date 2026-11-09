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
//! this test preserves the legacy structural checks without requiring a shell.
//! See docs/gmcp-roadmap.md for their scope and known limitations.

use std::path::Path;

#[path = "../xtask/src/gmcp_roadmap.rs"]
mod gmcp_roadmap;

#[test]
fn gmcp_roadmap_is_structurally_complete() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let errors = gmcp_roadmap::validate(root).expect("read native GMCP roadmap inputs");
    assert!(
        errors.is_empty(),
        "GMCP roadmap checker failed:\n{}",
        errors.join("\n")
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
