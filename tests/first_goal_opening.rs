#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

//! Exercise the real opening process: waiting for input must not initialize
//! the mission substrate, and closed input must not spin printing prompts.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use susi_paths::test_env::isolate_home;

fn opening(args: &[&str], input: &[u8]) -> (String, String) {
    let home = std::env::temp_dir().join(format!(
        "susi-first-goal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    command.env_clear();
    for name in ["PATH", "SystemRoot", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    isolate_home(&mut command, &home);
    command
        .args(args)
        .current_dir(&home)
        .env("SUSI_BOOT_TRACE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    drop(stdin);
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "opening exited with {status}");
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("opening must exit promptly without booting a mission or spinning on EOF");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    (stdout, stderr)
}

#[test]
fn first_goal_opening_exits_on_eof_without_booting_catalogs() {
    let (stdout, stderr) = opening(&["shell"], b"");
    assert!(
        stdout.contains("What would you like to achieve?"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("susi> ").count(), 1, "{stdout}");
    assert!(!stderr.contains("composition: cli"), "{stderr}");
    assert!(!stderr.contains("prime: frameworks"), "{stderr}");
}

#[test]
fn first_goal_opening_accepts_quit_without_starting_a_mission() {
    let (stdout, stderr) = opening(&["shell"], b"\nquit\n");
    assert_eq!(stdout.matches("susi> ").count(), 2, "{stdout}");
    assert!(!stdout.contains("Goal received"), "{stdout}");
    assert!(!stderr.contains("composition: cli"), "{stderr}");
}

#[test]
fn first_goal_opening_empty_pipe_does_not_boot_a_mission() {
    let (stdout, stderr) = opening(&[], b"");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(!stderr.contains("composition: cli"), "{stderr}");
}
