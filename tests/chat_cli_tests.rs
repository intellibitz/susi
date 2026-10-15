#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Tests for `susi chat` interactive conversational REPL CLI.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_susi_with_stdin(args: &[&str], input: &str) -> (i32, String, String) {
    let scratch = std::env::temp_dir().join(format!(
        "susi-chat-test-{}-{}",
        std::process::id(),
        args.join("_").replace(['/', ' ', '-'], "")
    ));
    let home = scratch.join("home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

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
    let _ = std::fs::remove_dir_all(&scratch);

    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn test_susi_chat_help_shows_options() {
    let (code, stdout, stderr) = run_susi_with_stdin(&["chat", "--help"], "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("--model"),
        "expected --model in help: {stdout}"
    );
    assert!(
        stdout.contains("--system"),
        "expected --system in help: {stdout}"
    );
}

#[test]
fn test_susi_chat_slash_exit_clean() {
    let (code, stdout, stderr) = run_susi_with_stdin(&["chat"], "/exit\n");
    assert_eq!(code, 0, "stderr: {stderr}, stdout: {stdout}");
}

#[test]
fn test_susi_chat_slash_commands_in_session() {
    let input = "/system You are a test bot\n/tokens\n/exit\n";
    let (code, stdout, stderr) = run_susi_with_stdin(&["chat"], input);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("Updated system instructions"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("Session Statistics:"), "stdout: {stdout}");
}
