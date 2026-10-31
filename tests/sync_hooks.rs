#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Mandate 49: any agent's git activity in a worktree keeps the primary
//! checkout's `main` current, through `.githooks/sync-primary`; and every
//! commit on a feature branch brings that branch up to date with
//! `origin/main` when the merge is clean, through `.githooks/sync-branch`
//! (sync after every feature, so a conflict is found one commit after it
//! arises and the push guard's sync-before-push is already satisfied).

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env_remove("SUSI_PRIMARY_SYNC_ACTIVE")
        // The sync hooks run `scripts/park-primary.sh`, the stub that execs
        // `susi workflow park-primary`: the binary under test.
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
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
        )
        .trim()
        .to_string(),
    )
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    #[cfg(windows)]
    {
        let status = Command::new("sh")
            .args(["-c", "chmod +x \"$1\"", "sh"])
            .arg(path)
            .status()
            .expect("Git for Windows must provide chmod");
        assert!(status.success(), "chmod +x {path:?}");
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out) = run(dir, args, &[]);
    assert_eq!(code, 0, "git {args:?}: {out}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    other: PathBuf,
}

impl World {
    /// bare server + primary on main (carrying the real scripts and sync
    /// hooks) + a worktree whose hooks are the sync hooks only.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-synchk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bare, primary, wt, other) = (
            root.join("server.git"),
            root.join("primary"),
            root.join("wt"),
            root.join("other"),
        );
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        std::fs::create_dir_all(primary.join(".githooks")).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        #[cfg(windows)]
        git(&primary, &["config", "core.filemode", "false"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (src, dst) in [
            ("scripts/park-primary.sh", "scripts/park-primary.sh"),
            ("scripts/posix-compat.sh", "scripts/posix-compat.sh"),
            (".githooks/sync-primary", ".githooks/sync-primary"),
            (".githooks/sync-branch", ".githooks/sync-branch"),
            (".githooks/post-commit", ".githooks/post-commit"),
            (".githooks/post-merge", ".githooks/post-merge"),
            (".githooks/post-checkout", ".githooks/post-checkout"),
        ] {
            let to = primary.join(dst);
            std::fs::copy(repo.join(src), &to).unwrap();
            make_executable(&to);
        }
        git(&primary, &["add", "-A"]);
        #[cfg(windows)]
        for path in [
            "scripts/park-primary.sh",
            ".githooks/post-commit",
            ".githooks/sync-branch",
        ] {
            git(&primary, &["update-index", "--chmod=+x", path]);
            assert!(
                git(&primary, &["ls-files", "-s", path]).starts_with("100755 "),
                "Windows fixture must preserve executable mode for {path}"
            );
        }
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "work",
                wt.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(&wt, &["config", "core.hooksPath", ".githooks"]);
        git(
            &root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        Self {
            root,
            primary,
            wt,
            other,
        }
    }

    fn origin_advances(&self) {
        git(
            &self.other,
            &["commit", "--allow-empty", "--quiet", "-m", "newer"],
        );
        git(&self.other, &["push", "--quiet", "origin", "HEAD:main"]);
    }

    fn commit_in_worktree(&self, envs: &[(&str, &str)]) -> i32 {
        run(
            &self.wt,
            &["commit", "--allow-empty", "--quiet", "-m", "work"],
            envs,
        )
        .0
    }

    /// `origin/main` gains a commit that writes `path`.
    fn origin_advances_with(&self, path: &str, body: &str) {
        std::fs::write(self.other.join(path), body).unwrap();
        git(&self.other, &["add", path]);
        git(&self.other, &["commit", "--quiet", "-m", "newer file"]);
        git(&self.other, &["push", "--quiet", "origin", "HEAD:main"]);
    }

    /// A commit in the worktree that writes `path`, with the hook's output.
    fn commit_file_in_worktree(
        &self,
        path: &str,
        body: &str,
        envs: &[(&str, &str)],
    ) -> (i32, String) {
        std::fs::write(self.wt.join(path), body).unwrap();
        git(&self.wt, &["add", path]);
        run(&self.wt, &["commit", "--quiet", "-m", "work"], envs)
    }

    fn worktree_head(&self) -> String {
        git(&self.wt, &["rev-parse", "HEAD"])
    }

    /// Whether the worktree's HEAD contains the server's `main`.
    fn worktree_contains_server_main(&self) -> bool {
        let main = self.server_main();
        run(
            &self.wt,
            &["merge-base", "--is-ancestor", &main, "HEAD"],
            &[],
        )
        .0 == 0
    }

    /// Nothing uncommitted and no merge left open.
    fn worktree_is_clean(&self) -> bool {
        git(&self.wt, &["status", "--porcelain"]).is_empty()
            && run(
                &self.wt,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
                &[],
            )
            .0 != 0
    }

    fn primary_head(&self) -> String {
        git(&self.primary, &["rev-parse", "HEAD"])
    }

    fn server_main(&self) -> String {
        git(&self.other, &["rev-parse", "origin/main"])
    }

    /// Keep wall-clock throttling deterministic even when the test host is
    /// under enough load for a test to be descheduled past the 30-second
    /// default between hook invocations.
    fn fixed_clock_path(&self) -> String {
        let bin = self.root.join("fixed-clock-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let date = bin.join("date");
        std::fs::write(&date, "#!/bin/sh\necho 1000\n").unwrap();
        make_executable(&date);
        let paths = std::iter::once(bin)
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        std::env::join_paths(paths)
            .unwrap()
            .into_string()
            .expect("PATH must be valid Unicode for the test command")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const FG: (&str, &str) = ("SUSI_SYNC_FOREGROUND", "1");

#[test]
fn hooks_sync_primary_after_a_commit_in_any_worktree() {
    let w = World::new("commit");
    w.origin_advances();
    let before = w.primary_head();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert_ne!(w.primary_head(), before, "primary main should have moved");
    assert_eq!(
        w.primary_head(),
        git(&w.primary, &["rev-parse", "origin/main"])
    );
    assert_eq!(w.primary_head(), w.server_main());
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
}

#[test]
fn hooks_sync_primary_is_throttled_and_the_interval_is_configurable() {
    let w = World::new("throttle");
    let fixed_path = w.fixed_clock_path();
    let clock = ("PATH", fixed_path.as_str());
    w.origin_advances();
    assert_eq!(w.commit_in_worktree(&[FG, clock]), 0);
    let synced = w.primary_head();
    w.origin_advances();
    // Within the default 30s window the second commit does not fetch again.
    assert_eq!(w.commit_in_worktree(&[FG, clock]), 0);
    assert_eq!(w.primary_head(), synced, "throttled: no second sync");
    assert_eq!(
        w.commit_in_worktree(&[FG, clock, ("SUSI_SYNC_INTERVAL", "0")]),
        0
    );
    assert_eq!(w.primary_head(), w.server_main(), "interval 0 syncs again");
}

#[test]
fn hooks_sync_primary_never_fails_or_moves_unsafe_state() {
    let w = World::new("safe");
    w.origin_advances();
    // Dirty primary: no move, and the commit still succeeds.
    std::fs::write(w.primary.join("scratch.txt"), "wip").unwrap();
    git(&w.primary, &["add", "scratch.txt"]);
    let before = w.primary_head();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert_eq!(w.primary_head(), before);
    // Server unreachable: the commit still succeeds.
    git(&w.primary, &["reset", "--quiet"]);
    std::fs::remove_file(w.primary.join("scratch.txt")).unwrap();
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    assert_eq!(w.commit_in_worktree(&[FG, ("SUSI_SYNC_INTERVAL", "0")]), 0);
}

#[test]
fn hooks_sync_primary_is_wired_into_every_git_activity_hook() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks");
    for h in ["post-commit", "post-merge", "post-checkout", "pre-push"] {
        let text = std::fs::read_to_string(repo.join(h)).unwrap();
        assert!(text.contains("sync-primary"), "{h} must call sync-primary");
        // Windows has no exec bit — Git for Windows runs hooks via sh anyway.
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(repo.join(h))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "{h} must be executable");
        }
    }
}

#[test]
fn hooks_sync_branch_merges_origin_main_after_a_clean_commit() {
    let w = World::new("branch-clean");
    w.origin_advances_with("theirs.txt", "theirs\n");
    assert!(
        !w.worktree_contains_server_main(),
        "fixture: branch starts behind"
    );
    let (code, out) = w.commit_file_in_worktree("mine.txt", "mine\n", &[FG]);
    assert_eq!(code, 0, "{out}");
    assert!(
        w.worktree_contains_server_main(),
        "the commit's branch must now contain origin/main: {out}"
    );
    assert!(
        w.worktree_is_clean(),
        "the merge must be committed, not left open"
    );
    assert!(
        w.wt.join("theirs.txt").is_file(),
        "origin's file is in the tree"
    );
    assert!(
        w.wt.join("mine.txt").is_file(),
        "the commit's file is in the tree"
    );
    assert!(
        out.contains("sync-branch:") && out.contains("contains origin/main"),
        "the hook says what it did: {out}"
    );
    // The merge commit's first parent is the commit just made.
    let first_parent = git(&w.wt, &["rev-parse", "HEAD^1"]);
    let subject = git(&w.wt, &["log", "-1", "--format=%s", &first_parent]);
    assert_eq!(subject, "work");
    // Already current: the next commit merges nothing more.
    let head = w.worktree_head();
    let (code, out) = w.commit_file_in_worktree("more.txt", "more\n", &[FG]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("sync-branch:"),
        "nothing to sync: silent ({out})"
    );
    assert_eq!(
        git(&w.wt, &["rev-parse", "HEAD^1"]),
        head,
        "a plain commit, no merge"
    );
}

#[test]
fn hooks_sync_branch_aborts_a_conflicting_merge_and_says_merge_now() {
    let w = World::new("branch-conflict");
    w.origin_advances_with("shared.txt", "theirs\n");
    let (code, out) = w.commit_file_in_worktree("shared.txt", "mine\n", &[FG]);
    assert_eq!(code, 0, "the commit itself never fails: {out}");
    assert!(
        !w.worktree_contains_server_main(),
        "a conflicting merge is not applied"
    );
    assert!(
        w.worktree_is_clean(),
        "the aborted merge leaves no MERGE_HEAD and no conflict markers"
    );
    // Line endings normalised: the aborted merge checks the file out again,
    // and a host's `core.autocrlf` may write it back with CRLF.
    assert_eq!(
        std::fs::read_to_string(w.wt.join("shared.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "mine\n",
        "the tree is as the commit left it"
    );
    assert_eq!(
        git(&w.wt, &["log", "-1", "--format=%s"]),
        "work",
        "HEAD is the commit just made"
    );
    assert!(
        out.contains("conflicts") && out.contains("git merge origin/main"),
        "the agent is told to merge now: {out}"
    );
}

#[test]
fn hooks_sync_branch_stays_out_of_the_way() {
    let w = World::new("branch-skip");
    w.origin_advances_with("theirs.txt", "theirs\n");
    // Inside finish, and when told not to: no merge.
    for off in [("SUSI_SYNC_IN_FINISH", "1"), ("SUSI_NO_BRANCH_SYNC", "1")] {
        let (code, out) =
            w.commit_file_in_worktree("mine.txt", &format!("{}\n", off.0), &[FG, off]);
        assert_eq!(code, 0, "{out}");
        assert!(
            !w.worktree_contains_server_main(),
            "{}=1 skips the sync",
            off.0
        );
        assert!(!out.contains("sync-branch:"), "{out}");
    }
    // Unstaged changes to a tracked file: a merge would touch them, so none.
    std::fs::write(w.wt.join("mine.txt"), "edited later\n").unwrap();
    assert_eq!(w.commit_in_worktree(&[FG]), 0);
    assert!(
        !w.worktree_contains_server_main(),
        "a dirty tree is never merged into"
    );
    assert_eq!(
        std::fs::read_to_string(w.wt.join("mine.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "edited later\n"
    );
    git(&w.wt, &["checkout", "--", "mine.txt"]);
    // The primary checkout, on main: never (nobody works there).
    git(&w.primary, &["config", "core.hooksPath", ".githooks"]);
    std::fs::write(w.primary.join("primary.txt"), "x\n").unwrap();
    git(&w.primary, &["add", "primary.txt"]);
    let (code, out) = run(&w.primary, &["commit", "--quiet", "-m", "on main"], &[FG]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("sync-branch:"), "{out}");
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert!(
        run(&w.primary, &["rev-parse", "-q", "--verify", "HEAD^2"], &[]).0 != 0,
        "no merge commit on main"
    );
    // Offline: the commit still succeeds, and nothing is merged.
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    let (code, out) = w.commit_file_in_worktree("offline.txt", "x\n", &[FG]);
    assert_eq!(code, 0, "{out}");
    assert!(w.worktree_is_clean());
}

#[test]
fn hooks_sync_branch_is_wired_into_post_commit() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks");
    let text = std::fs::read_to_string(repo.join("post-commit")).unwrap();
    assert!(
        text.contains("sync-branch"),
        "post-commit must call sync-branch"
    );
    let hook = std::fs::read_to_string(repo.join("sync-branch")).unwrap();
    for word in [
        "SUSI_SYNC_IN_FINISH",
        "SUSI_NO_BRANCH_SYNC",
        "merge --abort",
        "origin/main",
    ] {
        assert!(hook.contains(word), "sync-branch must handle {word}");
    }
    #[cfg(unix)]
    {
        let mode = std::fs::metadata(repo.join("sync-branch"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "sync-branch must be executable");
    }
}
