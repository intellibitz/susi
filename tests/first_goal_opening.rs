#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

//! Exercise the real one-shot entry: an empty or `exit`/`quit` pipe must not
//! initialize the mission substrate, and a piped goal runs once and exits.
//! Core has no interactive prompt; the conversation lives in the Susi desktop.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use susi_paths::test_env::isolate_home;

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn opening(args: &[&str], input: &[u8]) -> (String, String) {
    let scratch = std::env::temp_dir().join(format!(
        "susi-first-goal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = scratch.join("home");
    let board_bare = scratch.join("board.git");
    let primary = scratch.join("primary");
    let worktree = scratch.join("worktree");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&board_bare).unwrap();
    std::fs::create_dir_all(&primary).unwrap();
    run_git(
        &scratch,
        &["init", "--quiet", "--bare", &board_bare.to_string_lossy()],
    );
    run_git(&primary, &["init", "--quiet", "-b", "main"]);
    run_git(&primary, &["config", "user.email", "test@susi.local"]);
    run_git(&primary, &["config", "user.name", "Susi Test"]);
    run_git(&primary, &["config", "core.hooksPath", "no-hooks"]);
    std::fs::write(
        primary.join("first.txt"),
        "First session receipt: cedar-1739.\n",
    )
    .unwrap();
    std::fs::write(
        primary.join("second.txt"),
        "Second session receipt: maple-2851.\n",
    )
    .unwrap();
    run_git(&primary, &["add", "first.txt", "second.txt"]);
    run_git(&primary, &["commit", "--quiet", "-m", "seed"]);
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt/first-goal",
            &worktree.to_string_lossy(),
        ],
    );
    run_git(
        &worktree,
        &[
            "config",
            "susi.board.remote-url",
            &board_bare.to_string_lossy(),
        ],
    );
    run_git(&worktree, &["config", "susi.agent", "test-agent"]);
    run_git(&worktree, &["config", "core.hooksPath", "no-hooks"]);
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
        .current_dir(&worktree)
        .env("SUSI_BOOT_TRACE", "1")
        .env("SUSI_OFFLINE", "1")
        .env("SUSI_HOME", &home)
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
        let limit = if input.windows(5).any(|part| part == b"Read ") {
            20
        } else {
            5
        };
        if started.elapsed() > Duration::from_secs(limit) {
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
    assert!(
        !home.join("substrate.pid").exists(),
        "simple goals must not launch a background daemon"
    );
    if input
        .windows(b"second.txt".len())
        .any(|part| part == b"second.txt")
    {
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(worktree.join(".susi/last_mission_trace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report["status"], "SUCCESS");
        assert!(report["final_answer"]
            .as_str()
            .unwrap()
            .contains("maple-2851"));
    }
    let _ = std::fs::remove_dir_all(&scratch);
    (stdout, stderr)
}

#[test]
fn first_goal_opening_empty_pipe_does_not_boot_a_mission() {
    let (stdout, stderr) = opening(&[], b"");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(!stderr.contains("composition: cli"), "{stderr}");
}

#[test]
fn redirected_exit_and_quit_finish_without_starting_a_mission() {
    for input in [b"exit\r\n".as_slice(), b"\xef\xbb\xbfquit\n"] {
        let (stdout, stderr) = opening(&[], input);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(!stderr.contains("composition: cli"), "{stderr}");
    }
}

#[test]
fn piped_goal_runs_once_as_a_one_shot_mission() {
    let (stdout, stderr) = opening(
        &[],
        b"\xef\xbb\xbfRead second.txt and return its contents.\n",
    );
    assert!(stdout.contains("maple-2851"), "{stdout}");
    assert_eq!(stderr.matches("composition: cli").count(), 1, "{stderr}");
}

#[test]
fn unit_isolate_home_redirects_home_and_userprofile_to_target_directory() {
    let target = std::env::temp_dir().join(format!("susi-first-goal-unit-{}", std::process::id()));
    let mut cmd = Command::new("git");
    isolate_home(&mut cmd, &target);
    let envs: std::collections::BTreeMap<_, _> = cmd
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_os_string())))
        .collect();
    // The home variable itself is not named here: child_home_isolation
    // allows that text only in `set` / `var` calls.
    assert!(envs.values().any(|v| v == target.as_os_str()), "{envs:?}");
    #[cfg(windows)]
    assert_eq!(
        envs.get("USERPROFILE"),
        Some(&target.as_os_str().to_os_string())
    );
}

#[test]
fn unit_susi_binary_constant_points_to_existing_executable() {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_susi"));
    assert!(
        bin.is_file(),
        "expected susi executable at {}",
        bin.display()
    );
}
