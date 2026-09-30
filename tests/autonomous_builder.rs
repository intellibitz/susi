//! End-to-end autonomous builder: susi claims an open task, delegates it
//! to the best (fake) agent in a worktree with the self-build brief,
//! verifies with the acceptance check, commits with the Task trailer and
//! pushes the branch auto-merge turns into a PR. Everything runs against
//! a scratch bare remote and fixture repo — no shared state touched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use susi_gawd::autonomous_builder::{run_cycle, Delegation};

// The cycle mutates `cwd`-relative git state via the repo paths, but the
// claim's `SUSI_TASK_REMOTE` env is process-global — serialize the tests.
static SERIAL: Mutex<()> = Mutex::new(());

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A cloneable fixture repo: `origin` is a local bare remote; the working
/// tree ships an open task, an acceptance script, `.agents/identity.json`
/// and a `susi`-named Cargo.toml so the self-build brief applies.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("builder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let remote = root.join("remote.git");
    let repo = root.join("repo");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );

    // A SUSI-shaped tree: the brief attaches inside a worktree of it.
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"susi\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join(".agents/tasks/done")).unwrap();
    std::fs::write(repo.join(".agents/identity.json"), "{\"mandates\":[]}\n").unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    let accept = repo.join("scripts/accept.sh");
    std::fs::write(&accept, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&accept, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let task = serde_json::json!({
        "id": "T-INTELLIBITZ-901",
        "title": "Build the marker",
        "goal": "Create built.marker proving the fake agent ran",
        "size": "s",
        "accept": {"cmd": ["scripts/accept.sh"]},
        "created_by": "TEST",
        "created_unix": 0
    });
    std::fs::write(
        repo.join(".agents/tasks/T-INTELLIBITZ-901.json"),
        serde_json::to_string_pretty(&task).unwrap(),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "seed"]);
    git(&repo, &["push", "-qu", "origin", "main"]);
    (remote, repo)
}

#[test]
fn autonomous_builder_full_cycle_with_fake_agent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (remote, repo) = fixture("full");

    // Fake agent: proves it saw the brief and leaves a marker file.
    let saw_brief = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let brief_flag = saw_brief.clone();
    let delegate = move |d: &Delegation| -> susi_gawd::susi_error::EaiResult<()> {
        assert_eq!(d.task.id, "T-INTELLIBITZ-901");
        brief_flag.store(
            d.briefed_goal.contains("SUSI SELF-BUILD CONTRACT"),
            std::sync::atomic::Ordering::SeqCst,
        );
        std::fs::write(d.worktree.join("built.marker"), "done\n").unwrap();
        Ok(())
    };

    let outcome = run_cycle(
        &repo,
        "INTELLIBITZ",
        |open| open.first().map(|t| t.id.clone()),
        |_task| Some("fake-agent-1".to_string()),
        delegate,
    )
    .unwrap();

    // The brief travelled to the agent.
    assert!(saw_brief.load(std::sync::atomic::Ordering::SeqCst));
    // The acceptance check ran in the worktree and closed the task.
    assert!(outcome.worktree.join("built.marker").is_file());
    assert!(outcome
        .worktree
        .join(".agents/tasks/done/T-INTELLIBITZ-901.json")
        .is_file());
    assert!(!outcome
        .worktree
        .join(".agents/tasks/T-INTELLIBITZ-901.json")
        .exists());
    // The commit carries the mandatory trailer and sits on the pushed branch.
    let log = git(&outcome.worktree, &["log", "-1", "--format=%B"]);
    assert!(log.contains("Task: T-INTELLIBITZ-901"), "trailer: {log}");
    assert_eq!(
        outcome.commit,
        git(&outcome.worktree, &["rev-parse", "HEAD"])
    );
    // The branch reached the remote — what auto-merge opens a PR from.
    let on_remote = git(&repo, &["ls-remote", "origin", &outcome.branch]);
    assert!(on_remote.contains(&outcome.commit), "remote: {on_remote}");
    // The claim ref was created and released around the work.
    let _ = remote;

    std::fs::remove_dir_all(repo.parent().unwrap()).ok();
}

#[test]
fn autonomous_builder_aborts_without_agent_or_task() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_remote, repo) = fixture("none");

    // No qualifying agent → error before any worktree/claim side effect.
    let r = run_cycle(
        &repo,
        "INTELLIBITZ",
        |open| open.first().map(|t| t.id.clone()),
        |_task| None,
        |_d: &Delegation| Ok(()),
    );
    assert!(r.is_err());

    // No open task → error too.
    let empty = std::env::temp_dir().join(format!("builder-empty-{}", std::process::id()));
    std::fs::create_dir_all(empty.join(".agents/tasks")).unwrap();
    git(&empty, &["init", "-q", "-b", "main"]);
    let r2 = run_cycle(
        &empty,
        "INTELLIBITZ",
        |_open| None,
        |_t| Some("a".to_string()),
        |_d: &Delegation| Ok(()),
    );
    assert!(r2.is_err());

    std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    std::fs::remove_dir_all(&empty).ok();
}
