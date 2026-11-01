//! `bounded_cmd` tests — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

// These imports serve the existing Unix fixtures (`sh`, `sleep`, `cat`).
// The Windows fixtures below use native Rust children and no shell.
#[cfg(unix)]
use crate::bounded_cmd::output_within;
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
#[test]
fn captures_output_and_closes_stdin() {
    // `cat` echoes stdin; with stdin closed it exits at once with nothing.
    let out = output_within(
        Command::new("sh").args(["-c", "echo out; echo err >&2; cat"]),
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

#[cfg(unix)]
#[test]
fn overdue_children_are_killed() {
    let started = Instant::now();
    let err =
        output_within(Command::new("sleep").arg("30"), Duration::from_millis(150)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(windows)]
#[test]
fn native_probe_has_no_visible_console_captures_streams_and_closes_stdin() {
    let root = std::env::temp_dir().join(format!(
        "susi-native-console-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let executable = root.join("probe.exe");
    let mut compiler =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
    compiler
        .args([
            "--edition=2021",
            "--crate-name",
            "susi_native_console_probe",
        ])
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native_console_probe.rs"),
        )
        .arg("-o")
        .arg(&executable);
    // Propagate the already-selected test compiler's sysroot; this environment
    // is not an independent release-admission mechanism.
    if let Some(expected) = option_env!("SUSI_VERIFIED_SYSROOT") {
        assert_eq!(
            std::env::var_os("SUSI_VERIFIED_SYSROOT").as_deref(),
            Some(std::ffi::OsStr::new(expected))
        );
    }
    if let Some(sysroot) = std::env::var_os("SUSI_VERIFIED_SYSROOT") {
        compiler.arg("--sysroot").arg(sysroot);
    }
    let compiled =
        crate::bounded_cmd::output_within(&mut compiler, std::time::Duration::from_secs(30))
            .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = crate::bounded_cmd::output_within(
        &mut std::process::Command::new(&executable),
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native-probe-output"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("native-probe-error"));
    std::fs::remove_file(&executable).unwrap();
    let symbols = executable.with_extension("pdb");
    if symbols.exists() {
        std::fs::remove_file(symbols).unwrap();
    }
    std::fs::remove_dir(root).unwrap();
}

#[cfg(windows)]
#[test]
fn native_overdue_child_is_killed_and_reaped() {
    const MARKER: &str = "SUSI_NATIVE_PROBE_LATE_MARKER";
    if let Some(marker) = std::env::var_os(MARKER) {
        std::thread::sleep(std::time::Duration::from_secs(2));
        std::fs::write(marker, b"child outlived timeout").unwrap();
        return;
    }
    let marker = std::env::temp_dir().join(format!(
        "susi-probe-late-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!("{module}::native_overdue_child_is_killed_and_reaped");
    let started = std::time::Instant::now();
    let error = crate::bounded_cmd::output_within(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test, "--nocapture"])
            .env(MARKER, &marker),
        std::time::Duration::from_millis(500),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    std::thread::sleep(std::time::Duration::from_millis(1800));
    assert!(!marker.exists(), "timed-out child remained alive");
}
