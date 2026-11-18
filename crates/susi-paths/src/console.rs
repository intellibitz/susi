//! Unattended command launch helpers (`background_command`, `without_window`,
//! `apply_non_interactive_env`).

use std::ffi::OsStr;
use std::process::Command;

/// `CREATE_NO_WINDOW` (`0x0800_0000`): the child gets a console without a
/// window. Ignored for GUI programs; Windows only.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A [`Command`] for `program` that never opens a console window.
#[must_use]
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    without_window(&mut command);
    command
}

/// Make an existing `command` start without a console window (Windows); a
/// no-op elsewhere.
pub fn without_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Canonical environment pairs that suppress interactive terminal, SSH, GPG/GCM GUI,
/// and cloud-CLI prompts across Linux, macOS, Windows, and containers.
pub const NON_INTERACTIVE_ENV: [(&str, &str); 6] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GCM_INTERACTIVE", "never"),
    ("SSH_ASKPASS_REQUIRE", "never"),
    ("CLOUDSDK_CORE_DISABLE_PROMPTS", "1"),
    ("AWS_PAGER", ""),
    ("AZURE_CORE_NO_COLOR", "1"),
];

/// Apply [`NON_INTERACTIVE_ENV`] (and set `GIT_SSH_COMMAND` to `ssh -o BatchMode=yes`
/// if not already configured by the caller) so child processes never stall waiting on a human.
pub fn apply_non_interactive_env(command: &mut Command) -> &mut Command {
    for (k, v) in NON_INTERACTIVE_ENV {
        command.env(k, v);
    }
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    command
}

#[cfg(test)]
// Test-only assertions: a failed spawn or read must fail the test loudly.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_background_command_runs_and_captures_output() {
        let mut command = background_command("sh");
        command.args(["-c", "echo hidden"]);
        let out = command.output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hidden");
    }
}
