// Mixed native/static and optional POSIX compatibility checks.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Release mechanics that run outside GitHub Actions: Windows promotion. The
//! release workflow files and the PR merge bot that tagged merged version
//! bumps were removed (CEO decision 2026-11-06).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
// Executes a legacy POSIX script/hook; native coverage remains enabled.
#[cfg(unix)]
fn windows_release_promotion() {
    // Run the real Windows process and file mechanics where they exist; the
    // same entrypoint checks shell syntax on other release builders.
    #[cfg(windows)]
    let bash = {
        let git = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
            .unwrap();
        PathBuf::from(String::from_utf8_lossy(&git.stdout).trim())
            .ancestors()
            .nth(3)
            .unwrap()
            .join("bin/bash.exe")
    };
    #[cfg(not(windows))]
    let bash = PathBuf::from("bash");
    let output = std::process::Command::new(bash)
        .arg("scripts/test-release-sync-windows.sh")
        .current_dir(root())
        .output()
        .expect("run release promotion regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
