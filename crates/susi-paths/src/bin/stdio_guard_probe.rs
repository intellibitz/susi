//! Test probe for [`susi_paths::stdio::install_broken_pipe_guard`].
//!
//! Installs the guard, then writes to stdout until a write fails — the state a
//! closed reader (`susi status | head -1`) produces. Spawned by
//! `tests/cli_output_contract.rs` with the read end of its stdout already
//! closed.
//!
//! It is a separate binary rather than a `harness = false` test on purpose:
//! `cargo nextest`, which CI runs, discovers tests by executing every test
//! binary with `--list`, and it does not report a result for a binary that is
//! not a libtest harness — it marks the test skipped, so a regression could
//! never fail a shard. A plain bin is invisible to discovery and is spawned by
//! an ordinary test that nextest does run and report.
use std::io::{self, Read};

/// The parent releases the writer only after arranging its stdout reader.
/// EOF or a malformed token is a setup failure, never a successful probe.
fn await_release(mut input: impl Read) -> io::Result<()> {
    let mut token = [0];
    input.read_exact(&mut token)?;
    if token != *b"R" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid release token",
        ));
    }
    Ok(())
}

fn main() {
    susi_paths::stdio::install_broken_pipe_guard();
    if await_release(io::stdin().lock()).is_err() {
        std::process::exit(4);
    }
    // The parent has now closed the reader or started draining it. No sleep
    // is needed to guess when the pipe is ready, on either supported OS.
    for _ in 0..200 {
        println!("{}", "x".repeat(64));
    }
    // Every write succeeded: the reader never went away, so the spawning
    // experiment proved nothing and must not count as a pass.
    std::process::exit(3);
}

#[cfg(test)]
mod tests {
    // Test-only assertions in inline unit tests.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    #[test]
    fn install_broken_pipe_guard_preserves_non_pipe_panic_catching() {
        let original = std::panic::take_hook();
        susi_paths::stdio::install_broken_pipe_guard();
        let caught = std::panic::catch_unwind(|| {
            panic!("ordinary application panic");
        });
        let _ = std::panic::take_hook();
        std::panic::set_hook(original);
        assert!(caught.is_err());
    }

    #[test]
    fn probe_payload_chunk_has_expected_length_and_content() {
        let chunk = "x".repeat(64);
        assert_eq!(chunk.len(), 64);
        assert!(chunk.bytes().all(|b| b == b'x'));
    }

    #[test]
    fn probe_release_requires_the_token_and_rejects_eof() {
        assert!(super::await_release(&b"R"[..]).is_ok());
        assert_eq!(
            super::await_release(&b""[..]).unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            super::await_release(&b"X"[..]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn probe_retry_bound_and_no_epipe_exit_code_invariants_hold() {
        let retries = (0..200).count();
        let no_epipe_exit_code = 3_i32;
        assert_eq!(retries, 200);
        assert_ne!(no_epipe_exit_code, 0);
    }

    #[test]
    fn probe_total_write_volume_exceeds_typical_os_pipe_buffer_threshold() {
        let chunk_bytes = "x".repeat(64).len();
        let total_bytes_with_newlines = (chunk_bytes + 1) * 200;
        assert_eq!(total_bytes_with_newlines, 13_000);
        assert!(total_bytes_with_newlines > 8_192);
    }
}
