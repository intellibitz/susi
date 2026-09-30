//! A process whose stdout reader goes away must exit cleanly.
//!
//! Regression for `susi status | head -1`, which exited 134 (SIGABRT): Rust
//! turns the write's `EPIPE` into a panic, and the release profile aborts on
//! panic. The guard under test ends the process from the panic hook, so the
//! check cannot run on a test-harness thread — this file is `harness = false`
//! (see Cargo.toml) and owns `main`.
//!
//! `main` is both halves of the experiment: it spawns itself with the read end
//! of the child's stdout closed before the child writes, which is the state a
//! `| head -1` consumer leaves behind.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/// Set in the child. Unset in the parent, which runs the assertions.
const CHILD: &str = "SUSI_STDIO_GUARD_CHILD";

/// Exit status meaning "stdout accepted the write". Reaching it means the run
/// never reproduced the closed reader, so it must not count as a pass.
const NO_EPIPE: i32 = 3;

fn main() {
    if std::env::var_os(CHILD).is_some() {
        child();
    }
    parent();
}

/// Install the guard, then write to a stdout whose reader is already gone.
/// With the guard, the write's panic ends the process with status 0; without
/// it, the panic unwinds out of `main` and the process fails with 101.
fn child() {
    susi_paths::stdio::install_broken_pipe_guard();
    println!("{}", "x".repeat(64));
    // Only reachable when the write succeeded — the pipe was not closed.
    std::process::exit(NO_EPIPE);
}

fn parent() {
    let exe = std::env::current_exe().expect("current_exe");
    let mut spawned = std::process::Command::new(exe)
        .env(CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the guard child");
    // Close the read end before the child writes: its first stdout write then
    // has no reader at all and returns EPIPE.
    drop(spawned.stdout.take());
    let out = spawned
        .wait_with_output()
        .expect("wait for the guard child");
    let code = out.status.code();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code,
        Some(NO_EPIPE),
        "stdout accepted the write, so this run never saw EPIPE and proved nothing"
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
