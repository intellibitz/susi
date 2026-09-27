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
