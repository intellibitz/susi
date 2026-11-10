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
/// A test-only catalog restricts discovery to pinned fixture children; the
/// production scripts are unchanged and still refuse unreadable live images.
#[test]
#[cfg(windows)]
fn windows_release_promotion() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const BOUND: Duration = Duration::from_secs(300);
    // Windows PowerShell writes its module analysis cache under the known
    // LocalAppData folder; with the gate's isolated USERPROFILE that folder
    // does not exist and the cache lands in the working directory. Give the
    // run its own working directory and cache path so it never writes the tree.
    let scratch =
        std::env::temp_dir().join(format!("susi-release-promotion-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("create the regression's scratch directory");
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(root().join("scripts").join("test-release-sync-windows.ps1"))
        .current_dir(&scratch)
        .env(
            "PSModuleAnalysisCachePath",
            scratch.join("ModuleAnalysisCache"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the Windows release promotion regression");
    // Drain both pipes on their own threads so a full pipe cannot stall the
    // run, and read them with a deadline: a leftover grandchild may still hold
    // a pipe after the run itself was killed.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            let _ = sender.send(text);
        });
        receiver
    };
    let out = drain(Box::new(child.stdout.take().expect("stdout pipe")));
    let err = drain(Box::new(child.stderr.take().expect("stderr pipe")));
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
    let read = |pipe: mpsc::Receiver<String>| {
        pipe.recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| String::from("<output still held open after 30 s>"))
    };
    let out = read(out);
    let err = read(err);
    let _ = std::fs::remove_dir_all(&scratch);
    let status = status.unwrap_or_else(|| {
        panic!("release promotion regression did not finish within {BOUND:?}\n{out}\n{err}")
    });
    assert!(status.success(), "{out}\n{err}");
    for marker in [
        "PASS: owned process safety",
        "PASS: fixture catalog rejects outside PIDs",
        "PASS: unreadable live ownership fails closed",
        "PASS: leaf admission rejects mismatched identity",
    ] {
        assert!(
            out.contains(marker),
            "the regression exited 0 without reporting {marker}\n{out}\n{err}"
        );
    }
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
