use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

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

/// Initialize an isolated linked Git worktree backed by a bare Susi Native
/// Board remote in `temp_dir`, configured locally for parallel-safe tests.
pub(crate) fn init_board_workspace(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("susi-a2a-board-{tag}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let board_bare = root.join("board.git");
    let primary = root.join("primary");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&board_bare).expect("mkdir board");
    std::fs::create_dir_all(&primary).expect("mkdir primary");

    run_git(
        &root,
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
            &format!("wt/{tag}-{seq}"),
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
    run_git(&worktree, &["config", "susi.agent", "a2a-test-agent"]);
    run_git(&worktree, &["config", "core.hooksPath", "no-hooks"]);
    worktree
}
