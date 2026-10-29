#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Tests for `susi run` direct execution CLI.

use std::process::Command;
use susi_paths::test_env::isolate_home;

fn run_susi(args: &[&str]) -> (i32, String, String) {
    let scratch = std::env::temp_dir().join(format!(
        "susi-run-test-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '-'], "")
    ));
    let home = scratch.join("home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let out = isolate_home(&mut Command::new(env!("CARGO_BIN_EXE_susi")), &home)
        .args(args)
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .output()
        .unwrap();

    let _ = std::fs::remove_dir_all(&scratch);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn test_susi_run_help_shows_options() {
    let (code, stdout, stderr) = run_susi(&["run", "--help"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("--model"),
        "expected --model in help: {stdout}"
    );
    assert!(
        stdout.contains("--system"),
        "expected --system in help: {stdout}"
    );
    assert!(
        stdout.contains("--no-stream"),
        "expected --no-stream in help: {stdout}"
    );
    assert!(
        stdout.contains("--json"),
        "expected --json in help: {stdout}"
    );
}

#[test]
fn test_susi_run_empty_prompt_fails_cleanly() {
    let (code, stdout, stderr) = run_susi(&["run"]);
    assert_ne!(code, 0, "stdout: {stdout}");
    assert!(
        stderr.contains("No prompt provided") || stdout.contains("No prompt provided"),
        "expected error message about missing prompt, got: stderr={stderr}, stdout={stdout}"
    );
}
