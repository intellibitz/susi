// Native Windows promotion checks plus optional POSIX syntax checks.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Release mechanics that run outside GitHub Actions: Windows promotion. The
//! release workflow files and the PR merge bot that tagged merged version
//! bumps were removed (CEO decision 2026-11-06).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs the real Windows process and file mechanics of a release swap through
/// Windows PowerShell: an owned daemon whose leaf service sits in its
/// kill-on-close job, an active client, another installation, rollback and a
/// locked legacy duplicate. The regression fails, never hangs: the whole run is
/// bounded and a run past the bound is killed and reported.
#[test]
#[cfg(windows)]
fn windows_release_promotion() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const BOUND: Duration = Duration::from_secs(300);
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(root().join("scripts").join("test-release-sync-windows.ps1"))
        .current_dir(root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the Windows release promotion regression");
    // Drain both pipes on their own threads so a full pipe cannot stall the run.
    let mut out = child.stdout.take().expect("stdout pipe");
    let mut err = child.stderr.take().expect("stderr pipe");
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the regression") {
            break Some(status);
        }
        if started.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = out.join().expect("stdout reader");
    let err = err.join().expect("stderr reader");
    let status = status.unwrap_or_else(|| {
        panic!("release promotion regression did not finish within {BOUND:?}\n{out}\n{err}")
    });
    assert!(status.success(), "{out}\n{err}");
    assert!(
        out.contains("PASS: owned process safety"),
        "the regression exited 0 without reporting its checks\n{out}\n{err}"
    );
}

#[test]
// Executes a legacy POSIX script; Windows runs the native check above.
#[cfg(unix)]
fn windows_release_promotion_entrypoint_syntax() {
    // On other release builders the entrypoint checks shell syntax.
    let output = std::process::Command::new("bash")
        .arg("scripts/test-release-sync-windows.sh")
        .current_dir(root())
        .output()
        .expect("run release promotion regression");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
