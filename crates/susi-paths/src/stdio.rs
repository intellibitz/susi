//! Host stdio contract for command-line processes.
//!
//! A CLI whose reader goes away — `susi status | head -1` — must stop writing
//! and exit cleanly. It did not: Rust's runtime sets `SIGPIPE` to `SIG_IGN`,
//! so the write returns `EPIPE` and the standard library turns that into a
//! panic ("failed printing to stdout"), which under the release profile's
//! `panic = "abort"` is a `SIGABRT`. `susi status | head -1` exited 134
//! instead of finishing, and every `susi <command> | head` pipeline with it.
//!
//! [`install_broken_pipe_guard`] is the interception point that a
//! `#![forbid(unsafe_code)]` binary can still use: the panic hook runs before
//! the abort, so ending the process there prevents it. The two alternatives
//! are both out of reach — restoring the default `SIGPIPE` disposition needs
//! `sigaction`, and `#[unix_sigpipe = "sig_dfl"]` is not on stable.

/// Exit cleanly instead of aborting when a stdout/stderr write finds its
/// reader gone.
///
/// Call this before writing anything, from a binary whose stdout may be a
/// pipe. A closed reader is not this process's failure — the work is done and
/// the reader simply stopped reading — so the process exits `0` without a
/// panic report, which is what a consumer like `head` expects from a
/// well-behaved tool.
///
/// The guard chains to the previous hook, so every other panic still reports
/// as it did before.
pub fn install_broken_pipe_guard() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_broken_pipe_message(panic_message(info)) {
            // Terminating here is also what keeps `panic = "abort"` from
            // turning a handled broken pipe into a SIGABRT.
            std::process::exit(0);
        }
        previous(info);
    }));
}

/// True when a panic is a stdio write failing because the reader is gone.
/// A panic hook cannot inspect the write's `io::Error`, only the message the
/// standard library panicked with, so match its marker plus the OS error —
/// a non-pipe write failure (`Permission denied`) must keep reporting.
pub(crate) fn is_broken_pipe_message(message: &str) -> bool {
    message.contains("failed printing to")
        && (message.contains("Broken pipe")
            || message.contains("os error 32")
            || message.contains("The pipe is being closed")
            || message.contains("os error 232")
            || message.contains("The pipe has been ended")
            || message.contains("os error 109"))
}

/// The message a panic hook was handed, for either `panic!` payload form.
fn panic_message<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    if let Some(text) = info.payload().downcast_ref::<&str>() {
        return text;
    }
    if let Some(text) = info.payload().downcast_ref::<String>() {
        return text.as_str();
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::is_broken_pipe_message;

    #[test]
    fn cli_output_contract_classifies_a_broken_pipe_print_panic() {
        for message in [
            "failed printing to stdout: Broken pipe (os error 32)",
            "failed printing to stderr: Broken pipe (os error 32)",
            "failed printing to stdout: The pipe is being closed. (os error 232)",
            "failed printing to stderr: The pipe has been ended. (os error 109)",
        ] {
            assert!(is_broken_pipe_message(message), "{message}");
        }
        // Unrelated and non-pipe failures keep the default hook's report.
        for message in [
            "assertion `left == right` failed",
            "failed printing to stdout: Permission denied (os error 13)",
        ] {
            assert!(!is_broken_pipe_message(message), "{message}");
        }
    }

    #[test]
    fn broken_pipe_markers_require_the_print_failure_prefix() {
        for message in [
            "",
            "Broken pipe",
            "Broken pipe (os error 32)",
            "os error 32",
            "write failed: The pipe has been ended. (os error 109)",
            // The standard library's marker is lower-case; a different
            // spelling is not its print failure.
            "Failed Printing To stdout: Broken pipe (os error 32)",
            "failed printing to stdout: broken pipe",
        ] {
            assert!(!is_broken_pipe_message(message), "{message:?}");
        }
        assert!(is_broken_pipe_message(
            "failed printing to stdout: os error 32"
        ));
        assert!(is_broken_pipe_message(
            "failed printing to stderr: os error 232"
        ));
        assert!(is_broken_pipe_message(
            "failed printing to stdout: os error 109"
        ));
    }

    /// The guard must hand every non-pipe panic to the hook that was
    /// installed before it, for both `&str` and `String` payloads.
    #[test]
    fn the_guard_chains_non_pipe_panics_to_the_previous_hook() {
        use std::sync::{Arc, Mutex};
        static HOOK_LOCK: Mutex<()> = Mutex::new(());
        let _serial = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        const MARK: &str = "stdio-guard-chain-probe";
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let original = std::panic::take_hook();
        let sink = Arc::clone(&seen);
        std::panic::set_hook(Box::new(move |info| {
            let text = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            if text.contains(MARK) {
                sink.lock().unwrap().push(text);
            }
        }));
        super::install_broken_pipe_guard();

        let static_payload = std::panic::catch_unwind(|| {
            std::panic::panic_any("stdio-guard-chain-probe: static");
        });
        let owned_payload = std::panic::catch_unwind(|| {
            std::panic::panic_any(format!("{MARK}: owned {}", 1));
        });

        // Restore the hook chain before asserting so a failure here cannot
        // leave the guard installed for other tests.
        let _ = std::panic::take_hook();
        std::panic::set_hook(original);

        assert!(static_payload.is_err() && owned_payload.is_err());
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![format!("{MARK}: static"), format!("{MARK}: owned 1")]
        );
    }

    #[test]
    fn is_broken_pipe_message_rejects_unrelated_os_errors_and_handles_surrounding_context() {
        for message in [
            "failed printing to stdout: Interrupted system call (os error 4)",
            "failed printing to stderr: Bad file descriptor (os error 9)",
            "failed printing to stdout: Resource temporarily unavailable (os error 11)",
        ] {
            assert!(!is_broken_pipe_message(message), "{message}");
        }
        for message in [
            "thread 'main' panicked at 'failed printing to stdout: Broken pipe (os error 32)'",
            "panicked: failed printing to stderr: The pipe is being closed (os error 232)",
        ] {
            assert!(is_broken_pipe_message(message), "{message}");
        }
    }

    #[test]
    fn is_broken_pipe_message_recognizes_both_stdout_and_stderr_across_all_supported_os_codes() {
        for stream in ["stdout", "stderr"] {
            for marker in [
                "Broken pipe",
                "os error 32",
                "The pipe is being closed",
                "os error 232",
                "The pipe has been ended",
                "os error 109",
            ] {
                let msg = format!("failed printing to {stream}: {marker}");
                assert!(is_broken_pipe_message(&msg), "{msg}");
            }
        }
    }
}
