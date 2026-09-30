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
        && (message.contains("Broken pipe") || message.contains("os error 32"))
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
}
