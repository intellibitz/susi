//! Host stdio contract for command-line processes.
//!
//! [`install_broken_pipe_guard`] intercepts `EPIPE` panics ("failed printing to
//! stdout") in the panic hook and exits `0` before `panic = "abort"` can turn a
//! closed reader (`susi status | head -1`) into `SIGABRT`.

/// Exit cleanly instead of aborting when a stdout/stderr write finds its reader gone.
pub fn install_broken_pipe_guard() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_broken_pipe_message(panic_message(info)) {
            std::process::exit(0);
        }
        previous(info);
    }));
}

pub(crate) fn is_broken_pipe_message(message: &str) -> bool {
    message.contains("failed printing to")
        && (message.contains("Broken pipe") || message.contains("os error 32"))
}

fn panic_message<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    if let Some(text) = info.payload().downcast_ref::<&str>() {
        return text;
    }
    if let Some(text) = info.payload().downcast_ref::<String>() {
        return text.as_str();
    }
    ""
}

/// Spawn a child on Linux (`Command::spawn`); Windows stdio-handle clearing
/// lives in Susi Commercial Apps (`susi-windows-runtime`).
pub fn spawn_without_inheriting_stdio(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::is_broken_pipe_message;

    #[test]
    fn cli_output_contract_classifies_a_broken_pipe_print_panic() {
        for message in [
            "failed printing to stdout: Broken pipe (os error 32)",
            "failed printing to stderr: Broken pipe (os error 32)",
            "failed printing to stdout: os error 32",
        ] {
            assert!(is_broken_pipe_message(message), "{message}");
        }
        for message in [
            "",
            "Broken pipe",
            "os error 32",
            "Failed Printing To stdout: Broken pipe (os error 32)",
            "assertion `left == right` failed",
            "failed printing to stdout: Permission denied (os error 13)",
        ] {
            assert!(!is_broken_pipe_message(message), "{message:?}");
        }
    }

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

        let _ = std::panic::take_hook();
        std::panic::set_hook(original);

        assert!(static_payload.is_err() && owned_payload.is_err());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![format!("{MARK}: static"), format!("{MARK}: owned 1")]
        );
    }
}
