//! Running external programs with a deadline. A probe (`tool --version`,
//! `docker info`) or peer CLI that hangs must not stall the caller, and a
//! timed-out child must not outlive the call.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs `cmd` to completion or `timeout`, whichever is first: stdin is
/// closed (a prompting program sees EOF), stdout/stderr are drained
/// concurrently, and on expiry the child is killed and reaped and
/// `ErrorKind::TimedOut` is returned.
/// Windows probes run without a console window. This helper owns their creation
/// flags; callers must not request an interactive console or different flags.
pub fn output_within(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
    // Redirected probes are unattended: no console window and non-interactive
    // env (one owner of the flags, `susi_paths::console`).
    susi_paths::console::without_window(cmd);
    susi_paths::console::apply_non_interactive_env(cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out after {timeout:?}; process killed"),
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    };
    let join = |h: std::thread::JoinHandle<Vec<u8>>| h.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout: join(stdout),
        stderr: join(stderr),
    })
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn echo_cmd(msg: &str) -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/C", &format!("echo {msg}")]);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sh");
            c.args(["-c", &format!("printf '%s\\n' '{msg}'")]);
            c
        }
    }

    #[test]
    fn output_within_captures_stdout_for_fast_command() {
        let mut cmd = echo_cmd("bounded-ok");
        let out = output_within(&mut cmd, Duration::from_secs(30)).unwrap();
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("bounded-ok"), "{text}");
    }

    #[test]
    fn output_within_returns_error_for_nonexistent_program() {
        let mut cmd = Command::new("susi-nonexistent-binary-xyz-99999");
        let err = output_within(&mut cmd, Duration::from_millis(500)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn output_within_captures_nonzero_exit_status() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 7"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 7"]);
            c
        };
        let out = output_within(&mut cmd, Duration::from_secs(30)).unwrap();
        assert_eq!(out.status.code(), Some(7));
    }

    #[test]
    fn output_within_drains_stderr_cleanly() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo err-msg 1>&2"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "printf 'err-msg\\n' >&2"]);
            c
        };
        let out = output_within(&mut cmd, Duration::from_secs(30)).unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("err-msg"), "{stderr}");
    }

    #[test]
    fn output_within_kills_and_returns_timed_out_for_slow_command() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 5 127.0.0.1 >nul"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 5"]);
            c
        };
        let err = output_within(&mut cmd, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }
}
