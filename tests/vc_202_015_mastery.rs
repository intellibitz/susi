#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! VC-202-015 mastery: a task the system proposed for itself is
//! implemented, gated, merged, released and promoted by the workflow,
//! with the whole loop measured — elapsed time, cost, defects — in a
//! reproducible scorecard a human can read and stop.
//!
//! `autonomous_builder::run_cycle` (crates/susi-gawd/src/autonomous_builder.rs)
//! covers propose → claim → delegate → gate (fmt/clippy/test) → commit →
//! close → push, proven here end to end against a scratch bare remote.
//! What the mastery_target also asks for and this cycle does not do:
//!
//! 1. Confirm the pushed branch actually lands on `origin/main` —
//!    `run_cycle` returns the instant `git push` succeeds; it never polls
//!    for the merge the way `susi workflow finish` does.
//! 2. Release and promote the result — nothing here calls
//!    `admin::release::execute_release` (the `susi admin release --cut`
//!    path) or `scripts/susi-release-sync.sh`; a closed, pushed task is
//!    never carried through to an installed, promoted binary.
//! 3. Measure the loop — `BuildOutcome` carries only `task_id`, `agent`,
//!    `worktree`, `branch`, `commit`; no elapsed time, cost or defect
//!    count is recorded anywhere, and no scorecard type for a build
//!    cycle exists in the crate to record one into.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use susi_gawd::autonomous_builder::{run_cycle, BuildOutcome, Delegation};

// The cycle mutates `cwd`-relative git state; serialize against the
// crate-level `tests/autonomous_builder.rs` suite sharing the same
// process-global git config state.
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

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("vc202015-{tag}-{}", std::process::id()));
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
        "id": "T-VC202015-1",
        "title": "Build the marker",
        "goal": "Create built.marker proving the fake agent ran",
        "size": "s",
        "accept": {"cmd": ["scripts/accept.sh"]},
        "created_by": "TEST",
        "created_unix": 0
    });
    std::fs::write(
        repo.join(".agents/tasks/T-VC202015-1.json"),
        serde_json::to_string_pretty(&task).unwrap(),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "seed"]);
    git(&repo, &["push", "-qu", "origin", "main"]);
    (remote, repo)
}

#[test]
fn vc_202_015_mastery() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (_remote, repo) = fixture("loop");

    let delegate = |d: &Delegation| -> susi_gawd::susi_error::EaiResult<()> {
        std::fs::write(d.worktree.join("built.marker"), "done\n").unwrap();
        Ok(())
    };

    let outcome: BuildOutcome = run_cycle(
        &repo,
        "INTELLIBITZ",
        |open| open.first().map(|t| t.id.clone()),
        |_task| Some("fake-agent-1".to_string()),
        delegate,
    )
    .expect("propose -> gate -> push must succeed end to end");

    // Holds: implemented, gated and pushed for auto-merge to pick up.
    assert!(outcome
        .worktree
        .join(".agents/tasks/done/T-VC202015-1.json")
        .is_file());
    let on_remote = git(&repo, &["ls-remote", "origin", &outcome.branch]);
    assert!(on_remote.contains(&outcome.commit), "remote: {on_remote}");

    // Missing: the cycle returns the instant the push succeeds — it
    // never confirms the branch actually reached origin/main (unlike
    // `susi workflow finish`'s await_merged-style poll), so "merged" is
    // not yet a property this call can observe, only "pushed".
    //
    // Missing: nothing here calls admin::release::execute_release or
    // scripts/susi-release-sync.sh, so a closed, pushed task is never
    // carried through to an installed, promoted binary.
    //
    // Missing: BuildOutcome carries no elapsed time, cost or defect
    // count, and the crate has no build-cycle scorecard type to record
    // one into — the loop runs, but nothing measures it.
    let _: Vec<&str> = vec![
        &outcome.task_id,
        &outcome.agent,
        &outcome.branch,
        &outcome.commit,
    ];

    std::fs::remove_dir_all(repo.parent().unwrap()).ok();
}
