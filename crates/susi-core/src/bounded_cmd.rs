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
pub fn output_within(cmd: &mut Command, timeout: Duration) -> std::io::Result<Output> {
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

/// A `Command` that runs a script on any host. Windows cannot exec a
/// shebang (`%1 is not a valid Win32 application`, os error 193), so the
/// interpreter is chosen explicitly: `.py` goes to Python, everything else
/// to the POSIX shell — identical behaviour where the interpreters exist,
/// and the only way a script runs at all on Windows.
/// `bash` is preferred (repo scripts use bashisms), then `sh`, then the
/// POSIX shell Git for Windows bundles beside `cmd/git.exe`.
pub fn script_command(script: &std::path::Path, args: &[&str]) -> Command {
    #[cfg(windows)]
    {
        if script.extension().is_some_and(|e| e == "py") {
            let mut cmd = Command::new(python());
            // Windows consoles default to cp1252; repo scripts print UTF-8.
            cmd.arg(script).args(args).env("PYTHONUTF8", "1");
            return cmd;
        }
    }
    #[cfg(unix)]
    {
        // Shebang + exec bit already route the interpreter.
        let mut cmd = Command::new(script);
        cmd.args(args);
        return cmd;
    }
    #[allow(unreachable_code)]
    {
        let mut cmd = Command::new(posix_shell());
        cmd.arg(script).args(args);
        cmd
    }
}

/// A Python interpreter for `.py` scripts on hosts without shebang exec:
/// `python3`, then `python`, then the Windows `py` launcher. Probed once —
/// each spawn costs real time on Windows, and PATH does not change
/// mid-process.
#[cfg(windows)]
fn python() -> std::path::PathBuf {
    static PY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PY.get_or_init(|| {
        for name in ["python3", "python", "py"] {
            let ok = Command::new(name)
                .args(["-c", "pass"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                return std::path::PathBuf::from(name);
            }
        }
        std::path::PathBuf::from("python")
    })
    .clone()
}

/// The POSIX shell to run scripts through: `bash` or `sh` on PATH, or Git
/// for Windows' bundled `usr/bin/sh.exe` found via `git --exec-path`.
/// Probed once — every `script_command` would otherwise pay one or more
/// process spawns on Windows.
pub fn posix_shell() -> std::path::PathBuf {
    static SH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    SH.get_or_init(posix_shell_uncached).clone()
}

fn posix_shell_uncached() -> std::path::PathBuf {
    for name in ["bash", "sh"] {
        let ok = Command::new(name)
            .args(["-c", "exit 0"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return std::path::PathBuf::from(name);
        }
    }
    #[cfg(windows)]
    {
        // `git --exec-path` resolves to <git>/mingw64/libexec/git-core; the
        // bundled POSIX shell lives in <git>/usr/bin or <git>/bin.
        if let Ok(out) = Command::new("git").arg("--exec-path").output() {
            if out.status.success() {
                let exec_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                for rel in ["../usr/bin/sh.exe", "../bin/sh.exe"] {
                    let candidate = std::path::Path::new(&exec_path).join(rel);
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
    }
    std::path::PathBuf::from("sh")
}
