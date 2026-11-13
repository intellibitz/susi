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
use std::io::{self, Read, Write};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

/// Returns `true` when the environment explicitly requests non-interactive / unattended operation.
#[must_use]
pub fn is_unattended_env() -> bool {
    for var in [
        "SUSI_UNATTENDED",
        "SUSI_NONINTERACTIVE",
        "NONINTERACTIVE",
        "CI",
    ] {
        if let Ok(val) = std::env::var(var) {
            let clean = val.trim().to_ascii_lowercase();
            if !clean.is_empty() && !matches!(clean.as_str(), "0" | "false" | "no" | "off") {
                return true;
            }
        }
    }
    false
}

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

/// Execute `command` (applying [`without_window`]), optionally feeding `stdin_bytes` or closing
/// stdin with `Stdio::null()`, draining stdout/stderr concurrently, and killing + reaping the
/// child cleanly if `timeout` elapses.
pub fn run_output_within(
    command: &mut Command,
    stdin_bytes: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Output> {
    without_window(command);
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let stdin_pipe = child.stdin.take();
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdin_owned = stdin_bytes.map(<[u8]>::to_vec);

    let writer = std::thread::spawn(move || -> io::Result<()> {
        if let (Some(mut bytes), Some(mut pipe)) = (stdin_owned, stdin_pipe) {
            let res = pipe.write_all(&bytes);
            bytes.fill(0);
            res?;
        }
        Ok(())
    });
    let out_reader = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stdout_pipe {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(mut r) = stderr_pipe {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = writer.join();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("child process timed out after {}s", timeout.as_secs()),
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(e);
            }
        }
    };

    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
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

    #[cfg(not(windows))]
    #[test]
    fn run_output_within_kills_hung_child_on_timeout() {
        let mut cmd = background_command("sh");
        cmd.args(["-c", "sleep 30"]);
        let err = run_output_within(&mut cmd, None, Duration::from_millis(120)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn run_output_within_pipes_stdin_and_captures_stdout() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = background_command("cmd");
            c.args(["/C", "findstr .*"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = background_command("cat");
        let out =
            run_output_within(&mut cmd, Some(b"hello unattended"), Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "hello unattended"
        );
    }
}
