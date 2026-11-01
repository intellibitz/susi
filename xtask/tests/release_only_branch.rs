#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Validate that the release-only shortcut fails closed for non-release changes.

use std::path::Path;
use std::process::Command;

#[test]
fn version_only_classifier_accepts_only_exact_release_changes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = Command::new(python)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests",
            "-p",
            "test_release_only_branch.py",
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(root)
        .output()
        .expect("Python is required for the release classifier test");
    assert!(
        output.status.success(),
        "classifier tests failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
