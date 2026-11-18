use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Provision an isolated linked Git worktree backed by a bare Susi Native Board
/// remote inside `root`, configured locally with `susi.board.remote-url` and
/// `susi.agent` so parallel test threads never share environment state.
pub(crate) fn init_worktree_with_board(root: &Path, branch: &str) -> PathBuf {
    let board_bare = root.join("board.git");
    let primary = root.join("primary");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&board_bare).expect("mkdir board");
    std::fs::create_dir_all(&primary).expect("mkdir primary");

    run_git(
        root,
        &["init", "--quiet", "--bare", &board_bare.to_string_lossy()],
    );
    run_git(&primary, &["init", "--quiet", "-b", "main"]);
    run_git(&primary, &["config", "user.email", "test@susi.local"]);
    run_git(&primary, &["config", "user.name", "Susi Test"]);
    run_git(&primary, &["config", "core.hooksPath", "no-hooks"]);
    std::fs::write(primary.join("README.md"), "seed\n").expect("write seed");
    run_git(&primary, &["add", "README.md"]);
    run_git(&primary, &["commit", "--quiet", "-m", "seed"]);
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
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
    worktree
}
