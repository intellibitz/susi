#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Hermetic tests for `susi code` high-speed coding CLI.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn run_susi_code(args: &[&str], input: &str) -> (i32, String, String) {
    let scratch = std::env::temp_dir().join(format!(
        "susi-code-test-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '-'], "")
    ));
    let home = scratch.join("home");
    let cwd = scratch.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
        let _ = stdin.flush();
    }

    let out = child.wait_with_output().unwrap();
    let _ = fs::remove_dir_all(&scratch);

    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn test_susi_code_help_shows_options() {
    let (code, stdout, stderr) = run_susi_code(&["code", "--help"], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("--model"),
        "expected --model in help: {stdout}"
    );
    assert!(
        stdout.contains("--file"),
        "expected --file in help: {stdout}"
    );
    assert!(
        stdout.contains("--diff-only"),
        "expected --diff-only in help: {stdout}"
    );
    assert!(stdout.contains("--yes"), "expected --yes in help: {stdout}");
    assert!(
        stdout.contains("--check"),
        "expected --check in help: {stdout}"
    );
    assert!(
        stdout.contains("--rollback-on-failure"),
        "expected --rollback-on-failure in help: {stdout}"
    );
    assert!(
        stdout.contains("--json"),
        "expected --json in help: {stdout}"
    );
}

#[test]
fn test_susi_code_no_instruction_errors() {
    let (code, _stdout, stderr) = run_susi_code(&["code"], "");
    assert_ne!(code, 0);
    assert!(
        stderr.contains("No instruction provided"),
        "expected error message on missing instruction: {stderr}"
    );
}
