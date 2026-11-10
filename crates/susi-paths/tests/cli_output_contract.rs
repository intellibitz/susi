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

use std::io::{Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Reap even when setup or an assertion fails. Only this test's child is killed.
struct Probe(Child);

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).expect("drain probe pipe");
        bytes
    })
}

fn run_probe(close_reader: bool) -> Output {
    let mut child = Probe(
        Command::new(env!("CARGO_BIN_EXE_stdio_guard_probe"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the stdio guard probe"),
    );
    let stdout = child.0.stdout.take().expect("piped stdout");
    let stdout = if close_reader {
        // This happens before the release token, not after a scheduling delay.
        drop(stdout);
        None
    } else {
        Some(drain(stdout))
    };
    let stderr = drain(child.0.stderr.take().expect("piped stderr"));
    let mut release = child.0.stdin.take().expect("piped stdin");
    release.write_all(b"R").expect("release the probe writer");
    drop(release);

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("probe exit status") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "stdio probe exceeded its deadline"
        );
        // Polling only bounds process supervision; the pipe handshake above
        // establishes ordering independently of scheduler speed.
        std::thread::sleep(Duration::from_millis(1));
    };
    Output {
        status,
        stdout: stdout.map_or_else(Vec::new, |reader| reader.join().expect("stdout reader")),
        stderr: stderr.join().expect("stderr reader"),
    }
}

/// Exit status the probe uses when it never saw a closed reader. Reaching it
/// means the run proved nothing, so it must not count as a pass.
const NO_EPIPE: i32 = 3;

#[test]
fn cli_output_contract_guard_exits_cleanly_on_a_closed_stdout() {
    let out = run_probe(true);
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
    let out = run_probe(false);
    assert_eq!(out.status.code(), Some(NO_EPIPE));
    assert!(!out.stdout.is_empty());
}

#[test]
fn stdio_guard_probe_drained_stdout_is_valid_utf8_with_newlines() {
    let out = run_probe(false);
    let text = String::from_utf8(out.stdout).expect("probe stdout must be UTF-8");
    assert_eq!(text.len(), 13_000);
    assert_eq!(text.lines().count(), 200);
    assert!(text.lines().all(|line| line == "x".repeat(64)));
    assert!(out.stderr.is_empty());
}
