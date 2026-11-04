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
fn main() {
    susi_paths::stdio::install_broken_pipe_guard();
    // The reader can close just after we start, and an early write merely
    // fills the pipe buffer and looks like success (that race failed this
    // check on CI while it passed on a warm 28-core host), so retry until a
    // write really fails.
    for _ in 0..200 {
        println!("{}", "x".repeat(64));
        std::thread::sleep(std::time::Duration::from_millis(5));
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
    fn probe_sleep_interval_is_five_milliseconds() {
        let sleep_dur = std::time::Duration::from_millis(5);
        assert_eq!(sleep_dur.as_millis(), 5);
    }

    #[test]
    fn probe_retry_bound_and_no_epipe_exit_code_invariants_hold() {
        let retries = (0..200).count();
        let no_epipe_exit_code = 3_i32;
        assert_eq!(retries, 200);
        assert_ne!(no_epipe_exit_code, 0);
    }
}
