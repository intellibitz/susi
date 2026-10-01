#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The git hooks are the loop's first enforcement layer — the commit trailer,
//! the primary-checkout refusal, sync-before-push — and nothing ever executed
//! them: only their wiring was asserted (that `pre-commit` calls
//! `workflow-guard commit`, and so on). A hook that silently stopped enforcing
//! would have gone unnoticed until a raw `git commit` landed something
//! malformed. These run the real scripts in real temporary repositories.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let out = command(dir, "git", args, "", &[]);
    assert_eq!(out.0, 0, "git {args:?}: {}", out.2);
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = command(dir, "git", args, "", &[]);
    assert_eq!(out.0, 0, "git {args:?}: {}", out.2);
    out.1.trim().to_string()
}

/// Runs `program` with `stdin` piped in; returns (code, stdout, stderr).
fn command(
    dir: &Path,
    program: &str,
    args: &[&str],
    stdin: &str,
    envs: &[(&str, &str)],
) -> (i32, String, String) {
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
    other: PathBuf,
}

impl Fixture {
    /// A repository with this repo's real hooks installed, plus a bare remote
    /// and a second clone (`other`) that shares it.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-hooks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let work = root.join("work");
        let other = root.join("other");
        for dir in [&bare, &work, &other] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        for dir in [&work, &other] {
            git(dir, &["init", "--quiet", "-b", "main"]);
            git(dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
            git(dir, &["commit", "--allow-empty", "--quiet", "-m", "init"]);
        }
        git(&work, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&other, &["fetch", "--quiet", "origin"]);
        git(&other, &["reset", "--hard", "--quiet", "origin/main"]);

        // The real hooks, and the scope checker commit-msg calls.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(work.join(".githooks")).unwrap();
        for name in ["commit-msg", "pre-commit", "pre-push", "workflow-guard"] {
            std::fs::copy(
                src.join(".githooks").join(name),
                work.join(".githooks").join(name),
            )
            .unwrap();
        }
        std::fs::create_dir_all(work.join("scripts")).unwrap();
        std::fs::copy(
            src.join("scripts/check-task-scope.py"),
            work.join("scripts/check-task-scope.py"),
        )
        .unwrap();
        // After the setup commit, so the hook does not judge it.
        git(&work, &["config", "core.hooksPath", ".githooks"]);
        Self { root, work, other }
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

#[test]
fn committing_in_the_primary_checkout_is_refused_unless_overridden() {
    let f = Fixture::new("primary");
    let (code, _, err) = hook(&f.work, "workflow-guard", &["commit"], "", &[]);
    assert_ne!(code, 0, "the primary checkout must refuse commits");
    assert!(
        err.contains("do not commit in the primary checkout"),
        "{err}"
    );
    assert!(err.contains("scripts/susi-worktree.sh"), "{err}");
    // The guidance is printed literally: an unquoted heredoc delimiter made the
    // backticks in it a command substitution, so bash syntax errors were
    // printed instead and `cd <path>` was swallowed.
    assert!(!err.contains("syntax error"), "{err}");
    assert!(err.contains("`cd <path>`"), "{err}");

    // On a branch of your own, the explicit override is what allows it.
    git(&f.work, &["switch", "--quiet", "-c", "work"]);
    let (code, _, err) = hook(&f.work, "workflow-guard", &["commit"], "", &[]);
    assert_ne!(code, 0, "still the primary checkout: {err}");
    let (code, _, err) = hook(
        &f.work,
        "workflow-guard",
        &["commit"],
        "",
        &[("SUSI_ALLOW_PRIMARY", "1")],
    );
    assert_eq!(code, 0, "{err}");
}

#[test]
fn pushing_a_branch_that_is_behind_origin_main_is_refused() {
    let f = Fixture::new("behind");
    // origin/main moves ahead of the pushing clone.
    git(
        &f.other,
        &["commit", "--allow-empty", "--quiet", "-m", "on main"],
    );
    git(&f.other, &["push", "--quiet", "origin", "HEAD:main"]);

    let lsha = git_out(&f.work, &["rev-parse", "HEAD"]);
    let zero = "0".repeat(40);
    let refspec = format!("refs/heads/work {lsha} refs/heads/work {zero}\n");
    let (code, _, err) = hook(&f.work, "workflow-guard", &["push"], &refspec, &[]);
    assert_ne!(code, 0, "sync-before-push must be enforced");
    assert!(err.contains("behind origin/main"), "{err}");
    assert!(err.contains("git merge origin/main"), "{err}");

    // After integrating it, the same push passes.
    git(&f.work, &["fetch", "--quiet", "origin"]);
    git(&f.work, &["merge", "--no-edit", "--quiet", "origin/main"]);
    let lsha = git_out(&f.work, &["rev-parse", "HEAD"]);
    let refspec = format!("refs/heads/work {lsha} refs/heads/work {zero}\n");
    let (code, _, err) = hook(&f.work, "workflow-guard", &["push"], &refspec, &[]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn deleting_or_force_pushing_main_is_refused() {
    let f = Fixture::new("main");
    let zero = "0".repeat(40);
    let main_sha = git_out(&f.work, &["rev-parse", "HEAD"]);

    // Deleting main.
    let (code, _, err) = hook(
        &f.work,
        "workflow-guard",
        &["push"],
        &format!("(delete) {zero} refs/heads/main {main_sha}\n"),
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("deleting main is forbidden"), "{err}");

    // Rewinding it: the remote side has a commit this push would drop. The
    // commit comes from the second clone because these very hooks (correctly)
    // refuse commits in a primary checkout.
    git(
        &f.other,
        &["commit", "--allow-empty", "--quiet", "-m", "ahead"],
    );
    let ahead = git_out(&f.other, &["rev-parse", "HEAD"]);
    let behind = git_out(&f.work, &["rev-parse", "HEAD"]);
    let (code, _, err) = hook(
        &f.work,
        "workflow-guard",
        &["push"],
        &format!("refs/heads/main {behind} refs/heads/main {ahead}\n"),
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("fast-forward"), "{err}");
}
