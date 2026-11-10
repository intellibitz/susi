//! The commit-message hook is the loop's first enforcement layer (every
//! commit names a `Task: T-<AGENT>-<n>` trailer of an open task), and only its
//! wiring used to be asserted. These run the real hook in real temporary
//! repositories. The primary-checkout refusal and the push rules are the
//! workflow guard (`susi-commercial workflow guard`, Susi Commercial Apps'
//! dev workflow since the lean-core move), tested there.
// Optional POSIX/hosted compatibility fixtures execute shell scripts.
// Native Windows delivery is covered by the Rust coordinator, bootstrap,
// installer and release suites; it must not require a POSIX userland.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let out = command(dir, "git", args, "", &[]);
    assert_eq!(out.0, 0, "git {args:?}: {}", out.2);
}

/// Runs `program` with `stdin` piped in; returns (code, stdout, stderr).
fn command(
    dir: &Path,
    program: &str,
    args: &[&str],
    stdin: &str,
    envs: &[(&str, &str)],
) -> (i32, String, String) {
    #[cfg(windows)]
    let mut cmd = if program.contains(".githooks") {
        let mut command = Command::new("sh");
        command.arg(program);
        command
    } else {
        Command::new(program)
    };
    #[cfg(not(windows))]
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("SUSI_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The real hook from this repository, run inside a temporary repo.
fn hook(
    repo: &Path,
    name: &str,
    args: &[&str],
    stdin: &str,
    envs: &[(&str, &str)],
) -> (i32, String, String) {
    command(
        repo,
        repo.join(".githooks").join(name).to_str().unwrap(),
        args,
        stdin,
        envs,
    )
}

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// A repository with this repo's real commit-message hook installed.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-hooks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "--quiet", "-b", "main"]);
        git(&work, &["commit", "--allow-empty", "--quiet", "-m", "init"]);

        // The real hooks, and every script they call.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(work.join(".githooks")).unwrap();
        std::fs::copy(
            src.join(".githooks/commit-msg"),
            work.join(".githooks/commit-msg"),
        )
        .unwrap();
        std::fs::create_dir_all(work.join("scripts")).unwrap();
        for name in ["check-task-scope.sh", "check-worker-identity.sh"] {
            std::fs::copy(
                src.join("scripts").join(name),
                work.join("scripts").join(name),
            )
            .unwrap();
        }
        // After the setup commit, so the hook does not judge it.
        git(&work, &["config", "core.hooksPath", ".githooks"]);
        Self { root, work }
    }

    fn message(&self, body: &str) -> String {
        let path = self.root.join("MSG");
        std::fs::write(&path, body).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn open_task(&self, id: &str) {
        std::fs::create_dir_all(self.work.join(".agents/tasks")).unwrap();
        std::fs::write(
            self.work.join(format!(".agents/tasks/{id}.json")),
            serde_json::json!({
                "id": id, "title": "x", "goal": "x", "size": "s",
                "accept": {"cmd": ["cargo", "--version"]},
                "created_by": "TEST", "created_unix": 0
            })
            .to_string(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_commit_message_without_a_task_trailer_is_refused() {
    let f = Fixture::new("msg");
    let msg = f.message("feat: do something\n");
    let (code, _, err) = hook(&f.work, "commit-msg", &[&msg], "", &[]);
    assert_ne!(code, 0, "a trailer-less commit must be refused: {err}");
    assert!(err.contains("no task"), "{err}");
    assert!(err.contains("Task: T-<AGENT>-<n>"), "{err}");
}

#[test]
fn a_trailer_naming_an_open_task_is_accepted() {
    let f = Fixture::new("trailer");
    f.open_task("T-TEST-1");
    let msg = f.message("feat: do something\n\nTask: T-TEST-1\n");
    let (code, _, err) = hook(&f.work, "commit-msg", &[&msg], "", &[]);
    assert_eq!(code, 0, "{err}");

    // A closed task cannot be cited again.
    std::fs::create_dir_all(f.work.join(".agents/tasks/done")).unwrap();
    std::fs::rename(
        f.work.join(".agents/tasks/T-TEST-1.json"),
        f.work.join(".agents/tasks/done/T-TEST-1.json"),
    )
    .unwrap();
    let (code, _, err) = hook(&f.work, "commit-msg", &[&msg], "", &[]);
    assert_ne!(code, 0);
    assert!(err.contains("already closed"), "{err}");
}
