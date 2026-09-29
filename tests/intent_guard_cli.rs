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
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env_remove("SUSI_HOME")
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
fn nested_command_word_is_refused_with_a_hint() {
    let (code, stderr, written) = run(&["release"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("susi admin release"), "{stderr}");
    assert!(written.is_empty(), "{written:?}");
}
