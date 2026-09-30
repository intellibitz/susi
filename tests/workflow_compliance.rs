#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/check-workflow-compliance.sh` against real temp repos and the real
//! `susi tasks` binary: a commit passes only if its `Task:` trailer names a
//! task that is closed or held by a live claim.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "agent")
        .env("GIT_AUTHOR_EMAIL", "a@a")
        .env("GIT_COMMITTER_NAME", "agent")
        .env("GIT_COMMITTER_EMAIL", "a@a")
        .env_remove("SUSI_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out.trim().to_string()
}

struct Env {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-wf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        let bare = root.join("server.git");
        for d in [&repo, &home, &bare] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet"]);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["commit", "--allow-empty", "-m", "init", "--quiet"]);
        git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "--quiet", "origin"]);
        Self { root, repo, home }
    }

    fn susi(&self, args: &[&str]) -> (i32, String, String) {
        run(
            &self.repo,
            env!("CARGO_BIN_EXE_susi"),
            args,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        )
    }

    fn commit(&self, file: &str, subject: &str, task: Option<&str>) -> String {
        std::fs::create_dir_all(self.repo.join("work")).unwrap();
        std::fs::write(self.repo.join("work").join(file), file).unwrap();
        git(&self.repo, &["add", "-A"]);
        match task {
            Some(t) => git(
                &self.repo,
                &[
                    "commit",
                    "--quiet",
                    "-m",
                    subject,
                    "-m",
                    &format!("Task: {t}"),
                ],
            ),
            None => git(&self.repo, &["commit", "--quiet", "-m", subject]),
        };
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn check(&self, base: &str) -> (i32, String) {
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh");
        let (code, _, err) = run(&self.repo, script.to_str().unwrap(), &[base, "HEAD"], &[]);
        (code, err)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_commit_must_name_a_claimed_or_closed_task() {
    let e = Env::new("main");
    // Install the rule (the script's own commit is the cutover and must comply).
    let (code, out, err) = e.susi(&["tasks", "add", "first", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-TEST-1"), "{out}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install the rule",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let origin_main = "origin/main";

    // 1. Trailer + live claim: passes.
    e.commit("a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check(origin_main);
    assert_eq!(code, 0, "{err}");

    // 2. No trailer: refused, and the message says how to fix it.
    let before = e.head();
    e.commit("b", "feat: b", None);
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("no 'Task: T-<AGENT>-<n>' trailer"), "{err}");

    // 3. Trailer naming a task that does not exist: refused.
    let before = e.head();
    e.commit("c", "feat: c", Some("T-TEST-99"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("not a task in this branch"), "{err}");

    // 4. A real task nobody has claimed: refused; claiming it makes it pass.
    let (code, _, err) = e.susi(&["tasks", "add", "second", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add second task"]);
    let before = e.head();
    e.commit("d", "feat: d", Some("T-TEST-2"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("nobody holds a claim"), "{err}");
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-2"]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");

    // 5. Work then close on the same branch passes: the task was open at the
    //    work commit and the branch closed it (ownership remains until merge).
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-2"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    let before = e.head();
    e.commit("e", "fix: e", Some("T-TEST-1"));
    let (code, _, err) = e.susi(&["tasks", "close", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "close first task"]); // task-only: exempt
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");

    // 5b. Once closed, a task cannot be cited again (unrelated work must not
    //     ride on it).
    let before = e.head();
    e.commit("e2", "fix: sneaky", Some("T-TEST-1"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("already closed"), "{err}");

    // 6. Release commits need no task.
    let before = e.head();
    e.commit("f", "chore: release v9.9.9", None);
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_closed_task_cannot_be_cited_again() {
    let e = Env::new("closed");
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install rule",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    e.commit("work", "feat: work", Some("T-TEST-1"));
    let (code, _, err) = e.susi(&["tasks", "close", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "close"]);
    let (code, err) = e.check("origin/main");
    assert_eq!(code, 0, "work then close on one branch is compliant: {err}");
    // A later commit citing the now-closed task is refused.
    let before = e.head();
    e.commit("more", "feat: more", Some("T-TEST-1"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("already closed"), "{err}");
}

#[test]
fn history_before_the_rule_is_not_judged() {
    let e = Env::new("cutover");
    // A commit with no trailer exists before the script does.
    e.commit("old", "feat: old work", None);
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    let (code, _, err) = e.susi(&["tasks", "add", "t", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install rule and task",
            "-m",
            "Task: T-TEST-1", // the commit that installs the rule is itself bound by it
        ],
    );
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    e.commit("new", "feat: new", Some("T-TEST-1"));
    let (code, err) = e.check("origin/main");
    assert_eq!(
        code, 0,
        "old untrailered history must not fail the check: {err}"
    );
}
