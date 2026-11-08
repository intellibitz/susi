#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! A mistyped command must never become an autonomous mission: the real
//! binary refuses it (exit 2) before any daemon, inference or file write.

use std::process::Command;
use susi_paths::test_env::isolate_home;

fn run(args: &[&str]) -> (i32, String, Vec<String>) {
    let scratch = std::env::temp_dir().join(format!(
        "susi-intent-guard-{}-{}",
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
    let written: Vec<String> = std::fs::read_dir(&cwd)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let _ = std::fs::remove_dir_all(&scratch);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        written,
    )
}

#[test]
fn flag_shaped_input_is_refused_without_side_effects() {
    let (code, stderr, written) = run(&["release", "--help"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("looks like a command"), "{stderr}");
    assert!(
        written.is_empty(),
        "guard must write nothing, found {written:?}"
    );
}

#[test]
fn retired_interactive_shell_is_refused_with_a_pointer() {
    let (code, stderr, written) = run(&["shell"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("Susi desktop app"), "{stderr}");
    assert!(stderr.contains("one-shot goals"), "{stderr}");
    assert!(written.is_empty(), "{written:?}");
}

#[test]
fn help_lists_no_interactive_shell_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("Usage:"), "{help}");
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("shell ")),
        "{help}"
    );
}

#[test]
fn nested_command_word_is_refused_with_a_hint() {
    let (code, stderr, written) = run(&["release"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("susi-commercial admin release"), "{stderr}");
    assert!(written.is_empty(), "{written:?}");
}

#[test]
fn unit_susi_binary_exists_for_intent_guard() {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_susi"));
    assert!(
        bin.is_file(),
        "expected susi executable at {}",
        bin.display()
    );
}
