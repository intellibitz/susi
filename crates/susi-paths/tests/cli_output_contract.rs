//! A process whose stdout reader goes away must exit cleanly.
//!
//! Regression for `susi status | head -1`, which exited 134 (SIGABRT): Rust
//! turns the write's `EPIPE` into a panic, and the release profile aborts on
//! panic. The guard ends the process from the panic hook, so the check cannot
//! run on a test-harness thread. It spawns `stdio_guard_probe` instead — a
//! separate binary that installs the guard and writes to a stdout whose reader
//! this test has already closed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

/// Exit status the probe uses when it never saw a closed reader. Reaching it
/// means the run proved nothing, so it must not count as a pass.
const NO_EPIPE: i32 = 3;

#[test]
fn cli_output_contract_guard_exits_cleanly_on_a_closed_stdout() {
    let mut spawned = std::process::Command::new(env!("CARGO_BIN_EXE_stdio_guard_probe"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the stdio guard probe");
    // Close the read end so the probe's writes have no reader at all.
    drop(spawned.stdout.take());
    let out = spawned
        .wait_with_output()
        .expect("wait for the guard probe");
    let code = out.status.code();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code,
        Some(NO_EPIPE),
        "stdout accepted every write, so this run never saw EPIPE and proved nothing"
    );
    assert_eq!(
        code,
        Some(0),
        "a closed stdout reader must exit 0, not fail; got {code:?} (stderr: {stderr})"
    );
    assert!(
        !stderr.contains("panicked"),
        "the broken-pipe guard must not report a panic: {stderr}"
    );
}

#[test]
fn no_epipe_sentinel_exit_code_is_distinct_from_success() {
    assert_eq!(NO_EPIPE, 3);
    assert_ne!(NO_EPIPE, 0);
}

#[test]
fn stdio_guard_probe_binary_path_is_present_on_disk() {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_stdio_guard_probe"));
    assert!(bin.is_file(), "expected probe binary at {}", bin.display());
}

#[test]
fn cli_output_contract_guard_exits_with_no_epipe_when_stdout_is_fully_drained() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stdio_guard_probe"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run the stdio guard probe to completion");
    assert_eq!(out.status.code(), Some(NO_EPIPE));
    assert!(!out.stdout.is_empty());
}

#[test]
fn stdio_guard_probe_drained_stdout_is_valid_utf8_with_newlines() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stdio_guard_probe"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run the stdio guard probe to completion");
    let text = String::from_utf8(out.stdout).expect("probe stdout must be UTF-8");
    assert!(text.contains('\n'));
    assert!(out.stderr.is_empty());
}
