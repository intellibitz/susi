// Mixed native/static and optional POSIX compatibility checks.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Worktree hygiene, through the real scripts and the binary.
//!
//! `susi workflow prune` (`scripts/prune-worktrees.sh` is its stub, which
//! execs it) is only allowed to reclaim a worktree whose work cannot be lost —
//! clean, and already contained in `origin/main` — and it must never remove
//! another agent's. `scripts/susi-worktree.sh` must refuse to reuse a
//! directory that belongs to a different agent, because two workers in one
//! tree share one token and one branch.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.arg(script).env("MSYS_NO_PATHCONV", "1");
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(script)
    }
}

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = if program.ends_with("prune-worktrees.sh") {
        let mut native = Command::new(env!("CARGO_BIN_EXE_susi"));
        native.args(["workflow", "prune"]);
        native
    } else if program.ends_with(".sh") {
        script_command(Path::new(program))
    } else {
        Command::new(program)
    };
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        // The scripts are stubs that exec `susi workflow prune` and the like:
        // the binary under test, not whatever susi the host has installed.
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
        .env_remove("SUSI_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let (code, out) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {out}");
}

fn script(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

struct World {
    root: PathBuf,
    repo: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-hygiene-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let repo = root.join("repo");
        for dir in [&bare, &repo] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        // Per-worktree config is what carries `susi.agent` in the real flow.
        git(&repo, &["config", "extensions.worktreeConfig", "true"]);
        git(&repo, &["commit", "--allow-empty", "--quiet", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "--quiet", "origin"]);
        Self { root, repo }
    }

    /// A linked worktree with its own branch, owner, and optional changes.
    fn worktree(&self, name: &str, owner: &str, kind: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                name,
                path.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(&path, &["config", "--worktree", "susi.agent", owner]);
        match kind {
            "finished" => {}
            "dirty" => std::fs::write(path.join("scratch.txt"), "uncommitted").unwrap(),
            "unmerged" => {
                std::fs::write(path.join("work.txt"), "not merged").unwrap();
                git(&path, &["add", "work.txt"]);
                git(&path, &["commit", "--quiet", "-m", "unmerged work"]);
            }
            other => panic!("unknown worktree kind {other}"),
        }
        path
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn prune_reclaims_only_work_that_cannot_be_lost() {
    let w = World::new("prune");
    // The runner holds the identity the script compares owners against.
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    let dirty = w.worktree("dirty", "TEST", "dirty");
    let unmerged = w.worktree("unmerged", "TEST", "unmerged");
    let other = w.worktree("other", "OTHER", "finished");
    let prune = script("prune-worktrees.sh");

    // Report first: the default must not touch anything.
    let (code, out) = run(&runner, &prune, &[], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("reclaimable"), "{out}");
    assert!(out.contains("uncommitted changes"), "{out}");
    assert!(out.contains("unmerged commits"), "{out}");
    assert!(out.contains("another agent"), "{out}");
    for path in [&finished, &dirty, &unmerged, &other] {
        assert!(path.exists(), "the dry run removed {path:?}");
    }

    // --apply reclaims this agent's finished worktree, and only that.
    let (code, out) = run(&runner, &prune, &["--apply"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !finished.exists(),
        "a merged, clean worktree is reclaimable"
    );
    assert!(dirty.exists(), "uncommitted work must be kept: {out}");
    assert!(unmerged.exists(), "unmerged commits must be kept: {out}");
    assert!(
        other.exists(),
        "another agent's worktree must be kept: {out}"
    );
    assert!(runner.exists(), "the worktree this ran from must be kept");

    // --all is the deliberate override for a shared clone.
    let (code, out) = run(&runner, &prune, &["--apply", "--all"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !other.exists(),
        "--all reclaims another agent's finished tree"
    );
    assert!(dirty.exists() && unmerged.exists() && runner.exists());
}

#[test]
fn prune_refuses_to_guess_when_origin_is_unreachable() {
    let w = World::new("offline");
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    let (code, out) = run(&runner, &script("prune-worktrees.sh"), &["--apply"], &[]);
    assert_ne!(code, 0, "an unreachable origin must refuse: {out}");
    assert!(out.contains("cannot reach origin"), "{out}");
    assert!(finished.exists(), "nothing may be removed while unsure");
}

/// Two workers in one directory share one token and one branch, so the reuse
/// path must refuse a directory that belongs to a different agent.
#[test]
// Executes a legacy POSIX script/hook; native coverage remains enabled.
#[cfg(unix)]
fn worktree_reuse_refuses_another_agents_directory() {
    let w = World::new("reuse");
    // The script derives the destination from the primary's name and the
    // worktree name, exactly as the fixture lays it out.
    let name = "taken";
    let path = w.root.join(format!(
        "{}-{name}",
        w.repo.file_name().unwrap().to_str().unwrap()
    ));
    git(
        &w.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            path.to_str().unwrap(),
            "origin/main",
        ],
    );
    git(&path, &["config", "--worktree", "susi.agent", "OTHERAGENT"]);

    let (code, out) = run(
        &w.repo,
        &script("susi-worktree.sh"),
        &[name],
        &[("SUSI_PRIMARY_WATCH_DISABLE", "1"), ("SUSI_AGENT", "test")],
    );
    assert_ne!(
        code, 0,
        "reusing another agent's directory must refuse: {out}"
    );
    assert!(out.contains("already belongs to agent OTHERAGENT"), "{out}");
    assert!(out.contains("One worktree per worker"), "{out}");
}

/// `susi workflow prune` as the binary, with the stub out of the way: the same
/// judgement, the same words, and a failed removal is a failed run.
#[test]
fn the_binary_prunes_as_the_script_did() {
    let w = World::new("binary");
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    let dirty = w.worktree("dirty", "TEST", "dirty");
    let other = w.worktree("other", "OTHER", "finished");
    let susi = env!("CARGO_BIN_EXE_susi");
    let (code, out) = run(&runner, susi, &["workflow", "prune"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("reclaimable"), "{out}");
    assert!(out.contains("uncommitted changes"), "{out}");
    assert!(out.contains("another agent"), "{out}");
    assert!(out.contains("dry-run"), "{out}");
    assert!(finished.exists() && dirty.exists() && other.exists());
    let (code, out) = run(&runner, susi, &["workflow", "prune", "--apply"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(!finished.exists(), "{out}");
    assert!(dirty.exists() && other.exists() && runner.exists(), "{out}");
    // A worktree whose directory is already gone from under git cannot be
    // judged: it is kept as unreadable, the run still succeeds, and git's
    // stale admin entry for it is pruned, as the script did on every run.
    let gone = w.worktree("gone", "TEST", "finished");
    std::fs::remove_dir_all(&gone).unwrap();
    let (code, out) = run(&runner, susi, &["workflow", "prune"], &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("unreadable"), "{out}");
    let (_, listed) = run(&runner, "git", &["worktree", "list"], &[]);
    assert!(
        !listed.contains("gone"),
        "the stale admin entry must be pruned: {listed}"
    );
    // --all reclaims the other agent's finished worktree too.
    let (code, out) = run(
        &runner,
        susi,
        &["workflow", "prune", "--apply", "--all"],
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert!(!other.exists(), "{out}");
    assert!(dirty.exists() && runner.exists());
}

/// The stub refuses an argument it does not know, as the script did, before
/// it looks for a susi.
#[test]
// Executes a legacy POSIX script/hook; native coverage remains enabled.
#[cfg(unix)]
fn the_prune_stub_refuses_an_unknown_argument() {
    let w = World::new("usage");
    let runner = w.worktree("runner", "TEST", "finished");
    let (code, out) = run(&runner, &script("prune-worktrees.sh"), &["--force"], &[]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("usage: prune-worktrees.sh"), "{out}");
}

/// On Windows an installed `susi.exe` reclaims worktrees with no Git userland
/// on PATH: only Git's `cmd` (git itself), System32 and the Windows
/// directory. The bash version needed `awk`, `jq` and `cygpath` from Git's
/// `usr/bin`. Returns at once where Git for Windows is not installed.
#[test]
fn the_prune_needs_no_git_userland_on_windows() {
    let Ok(out) = Command::new("git").arg("--exec-path").output() else {
        return;
    };
    let exec_path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let Some(git_root) = exec_path.ancestors().nth(3).map(Path::to_path_buf) else {
        return;
    };
    if !git_root.join("cmd").join("git.exe").is_file() {
        return; // not Git for Windows' layout
    }
    let windows = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let path = std::env::join_paths([
        git_root.join("cmd"),
        windows.join("System32"),
        windows.clone(),
    ])
    .unwrap();
    assert!(
        !path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("usr\\bin"),
        "the narrowed PATH must not carry Git's userland: {path:?}"
    );
    let w = World::new("nouserland");
    let runner = w.worktree("runner", "TEST", "finished");
    let finished = w.worktree("finished", "TEST", "finished");
    let dirty = w.worktree("dirty", "TEST", "dirty");
    let (code, out) = run(
        &runner,
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "prune", "--apply"],
        &[("PATH", path.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{out}");
    assert!(!finished.exists(), "{out}");
    assert!(dirty.exists(), "{out}");
}
