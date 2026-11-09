//! Console programs Susi launches unattended run without a console window.
//!
//! The daemon is started with `DETACHED_PROCESS`, so it has no console. A
//! console program it launches (`tasklist`, `netstat`, `taskkill`,
//! `nvidia-smi`, `git`, a cell binary) then gets a new console from Windows,
//! and that console opens a visible window on the operator's desktop for the
//! life of the child. Redirecting stdio does not prevent it; only
//! `CREATE_NO_WINDOW` does. This module is the one place that sets the flag
//! for unattended launches; it is std-only, so every crate down to
//! `susi-paths` can use it without a new dependency.
//!
//! Interactive commands (a CLI handing its own terminal to a child) do not
//! use it: a child of a process that owns a console shares that console and
//! never opens a window of its own.

use std::ffi::OsStr;
use std::process::Command;

/// `CREATE_NO_WINDOW` (`0x0800_0000`): the child gets a console without a
/// window. Ignored for GUI programs; Windows only.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A [`Command`] for `program` that never opens a console window: the way
/// every unattended launch (probe, port or process lookup, termination,
/// service or cell spawn) starts.
#[must_use]
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    without_window(&mut command);
    command
}

/// Make an existing `command` start without a console window (Windows); a
/// no-op elsewhere. This replaces any creation flags set before it.
pub fn without_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
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
        #[cfg(windows)]
        let mut command = {
            let mut c = background_command("cmd");
            c.args(["/C", "echo hidden"]);
            c
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut c = background_command("sh");
            c.args(["-c", "echo hidden"]);
            c
        };
        let out = command.output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hidden");
    }

    /// The child of a windowless launch has a console (so console programs
    /// work) that no window shows: `GetConsoleWindow` answers a handle, and
    /// that window is not visible. PowerShell reads both through Win32.
    #[cfg(windows)]
    #[test]
    fn a_background_child_has_no_visible_console_window() {
        let system = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        let powershell = system
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let script = "Add-Type -Name W -Namespace N -MemberDefinition \
            '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow(); \
             [DllImport(\"user32.dll\")] public static extern bool IsWindowVisible(System.IntPtr h);'; \
            $h = [N.W]::GetConsoleWindow(); \
            Write-Output (\"visible=\" + ($h -ne [System.IntPtr]::Zero -and [N.W]::IsWindowVisible($h)))";
        let mut child = background_command(powershell)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Bounded: a test fails, never hangs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("powershell probe did not finish within 120 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "visible=False",
            "{out:?}"
        );
    }
}
