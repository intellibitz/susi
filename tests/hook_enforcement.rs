//! The commit-message hook is the loop's first enforcement layer (every
//! commit names a `Task: T-<AGENT>-<n>` trailer of an open task), and only its
//! wiring used to be asserted. These run the real hook in real temporary
//! repositories. The primary-checkout refusal and the push rules are the
//! workflow guard (`susi-commercial workflow guard`, Susi Commercial Apps'
//! dev workflow since the lean-core move), tested there.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

mod common;

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
    // A hook runs as git runs it: under Git for Windows bash on Windows.
    let mut cmd = if program.contains(".githooks") {
        common::script_command(Path::new(program))
    } else {
        std::process::Command::new(program)
    };
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("SUSI_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = common::run_script_bounded(&mut cmd, stdin.as_bytes(), common::SCRIPT_BOUND);
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
        for name in [
            "commit-msg",
            "mandate-checks",
            "pre-push",
            "core-boundary-guard",
        ] {
            std::fs::copy(
                src.join(".githooks").join(name),
                work.join(".githooks").join(name),
            )
            .unwrap();
        }
        std::fs::write(work.join(".susi-repo-role"), "public-core\n").unwrap();
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

#[test]
fn standalone_pre_push_allows_ff_and_blocks_force_delete_tag_move_and_board_refs() {
    let f = Fixture::new("prepush");
    // Strip PATH down to system essentials without any push-policy binary, and
    // run in an isolated temp root with zero sibling commercial checkout.
    let empty_bin = f.root.join("no-tools");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let base_sha = command(&f.work, "git", &["rev-parse", "HEAD"], "", &[])
        .1
        .trim()
        .to_string();
    git(
        &f.work,
        &[
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "feat: standalone prepush fixture",
            "-m",
            "Task: LOCAL-prepush-fixture",
        ],
    );
    let head_sha = command(&f.work, "git", &["rev-parse", "HEAD"], "", &[])
        .1
        .trim()
        .to_string();
    let zero = "0000000000000000000000000000000000000000";

    // Filter out any directory containing push-policy from PATH so the
    // standalone pure-local fallback inside .githooks/pre-push always executes.
    let clean_path = std::env::var_os("PATH")
        .map(|raw| {
            std::env::join_paths(std::env::split_paths(&raw).filter(|dir| {
                !dir.join("push-policy").exists() && !dir.join("push-policy.exe").exists()
            }))
            .unwrap()
            .to_string_lossy()
            .into_owned()
        })
        .unwrap_or_default();
    let envs = [("PATH", clean_path.as_str())];

    // 1. Clean fast-forward push on main succeeds standalone.
    let ff = format!("refs/heads/main {head_sha} refs/heads/main {base_sha}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &ff,
        &envs,
    );
    assert_eq!(code, 0, "standalone FF push must succeed: {err}");

    // 2. Non-fast-forward (rewind) push is refused.
    let non_ff = format!("refs/heads/main {base_sha} refs/heads/main {head_sha}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &non_ff,
        &envs,
    );
    assert_ne!(code, 0, "non-FF push must be refused");
    assert!(
        err.contains("refusing the push (transport safety)"),
        "{err}"
    );

    // 3. Branch deletion is refused.
    let del_branch = format!("(delete) {zero} refs/heads/main {head_sha}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &del_branch,
        &envs,
    );
    assert_ne!(code, 0, "branch deletion must be refused");
    assert!(
        err.contains("refusing the push (transport safety)"),
        "{err}"
    );

    // 4. Moving or deleting an existing tag is refused; creating a new tag succeeds.
    let new_tag = format!("refs/tags/v0.1.0 {head_sha} refs/tags/v0.1.0 {zero}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &new_tag,
        &envs,
    );
    assert_eq!(code, 0, "new tag push must succeed: {err}");

    let move_tag = format!("refs/tags/v0.1.0 {head_sha} refs/tags/v0.1.0 {base_sha}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &move_tag,
        &envs,
    );
    assert_ne!(code, 0, "moving an existing tag must be refused");
    assert!(
        err.contains("refusing the push (transport safety)"),
        "{err}"
    );

    let del_tag = format!("(delete) {zero} refs/tags/v0.1.0 {head_sha}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &del_tag,
        &envs,
    );
    assert_ne!(code, 0, "deleting an existing tag must be refused");
    assert!(
        err.contains("refusing the push (transport safety)"),
        "{err}"
    );

    // 5. Private board refs are refused by core-boundary-guard.
    let board_name = ["refs", "board", "item-1"].join("/");
    let board_ref = format!("{board_name} {head_sha} {board_name} {zero}\n");
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &board_ref,
        &envs,
    );
    assert_ne!(code, 0, "private board ref push must be refused");
    assert!(err.contains("core-boundary-guard"), "{err}");

    // 6. Opt-in strict mode fails closed without push-policy.
    let strict_envs = [
        ("PATH", clean_path.as_str()),
        ("SUSI_REQUIRE_PUSH_POLICY", "1"),
    ];
    let (code, _, err) = hook(
        &f.work,
        "pre-push",
        &["origin", "file:///tmp/bare"],
        &ff,
        &strict_envs,
    );
    assert_ne!(
        code, 0,
        "SUSI_REQUIRE_PUSH_POLICY=1 must refuse when push-policy is absent"
    );
    assert!(
        err.contains("refusing the push (transport safety)"),
        "{err}"
    );
}
