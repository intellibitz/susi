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
