//! The built `local-snapshot` publisher composed with the built `push-policy`
//! helper, through the repository's own `.githooks/pre-push`, against real Git.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const PUBLISHER: &str = env!("CARGO_BIN_EXE_local-snapshot");
const POLICY: &str = env!("CARGO_BIN_EXE_push-policy");

/// The inherited search path without the build directory, so the hook can only
/// find `push-policy` through the publisher's adjacent-helper discovery.
fn path_without_helper() -> OsString {
    let helper_dir = Path::new(POLICY).parent().unwrap();
    let kept: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p != helper_dir)
        .collect();
    std::env::join_paths(kept).unwrap()
}

fn isolated<'a>(command: &'a mut Command, dir: &Path) -> &'a mut Command {
    command
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_GRAFT_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@localhost")
        .env("PATH", path_without_helper())
}

struct Fixture {
    root: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi-publisher-hook-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let hooks = root.join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-push");
        std::fs::write(&hook, include_str!("../../.githooks/pre-push")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let remote = root.join("remote.git");
        let f = Self { root, remote };
        f.git(
            &f.root,
            &["init", "-q", "--bare", "-b", "main", "remote.git"],
        );
        let seed = f.clone_worker("seed");
        f.commit(&seed, "seed.txt", "seed");
        f.push_main_with_policy(&seed);
        f
    }

    /// An ordinary push through the real hook, with the helper directory
    /// supplied explicitly on the search path, as an operator would.
    fn push_main_with_policy(&self, dir: &Path) {
        let mut path = vec![Path::new(POLICY).parent().unwrap().to_owned()];
        path.extend(std::env::split_paths(&path_without_helper()));
        let out = isolated(&mut Command::new("git"), dir)
            .env("PATH", std::env::join_paths(path).unwrap())
            .args(["push", "-q", "origin", "HEAD:refs/heads/main"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = isolated(&mut Command::new("git"), dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A clone whose pushes run the repository's real pre-push hook.
    fn clone_worker(&self, name: &str) -> PathBuf {
        let remote = self.remote.to_str().unwrap();
        self.git(&self.root, &["clone", "-q", remote, name]);
        let dir = self.root.join(name);
        let hooks = self.root.join("hooks");
        self.git(&dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
        self.git(&dir, &["config", "commit.gpgsign", "false"]);
        if self.git(&dir, &["branch", "--show-current"]).is_empty() {
            self.git(&dir, &["checkout", "-q", "-b", "main"]);
        }
        dir
    }

    fn commit(&self, dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        self.git(dir, &["add", file]);
        self.git(dir, &["commit", "-q", "-m", text]);
        self.git(dir, &["rev-parse", "HEAD"])
    }

    fn remote_main(&self) -> String {
        self.git(&self.remote, &["rev-parse", "refs/heads/main"])
    }

    fn contains(&self, ancestor: &str, head: &str) -> bool {
        isolated(&mut Command::new("git"), &self.remote)
            .args(["merge-base", "--is-ancestor", ancestor, head])
            .status()
            .unwrap()
            .success()
    }

    /// Advance remote main from a second worker, through the real hook.
    fn advance_main_elsewhere(&self, name: &str) -> String {
        let other = self.clone_worker(name);
        let commit = self.commit(&other, "other.txt", name);
        self.push_main_with_policy(&other);
        commit
    }

    fn publish(&self, binary: &Path, dir: &Path, args: &[&str]) -> Output {
        isolated(&mut Command::new(binary), dir)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Unique directory created by this fixture; no external path is accepted.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout {}\nstderr {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn built_publisher_and_policy_are_adjacent() {
    assert_eq!(Path::new(PUBLISHER).parent(), Path::new(POLICY).parent());
}

#[test]
fn submit_composes_divergent_main_through_the_real_hook_and_adjacent_policy() {
    let f = Fixture::new();
    let worker = f.clone_worker("worker");
    let source = f.commit(&worker, "worker.txt", "worker change");
    let other = f.advance_main_elsewhere("other");

    let out = f.publish(Path::new(PUBLISHER), &worker, &["submit", "hook-composed"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("is verified on main"));

    let main = f.remote_main();
    let parents = f.git(&f.remote, &["log", "-1", "--format=%P", &main]);
    assert_eq!(parents, format!("{other} {source}"));
    assert!(f.contains(&source, &main) && f.contains(&other, &main));
    // The worker checkout is never moved or dirtied by composition.
    assert_eq!(f.git(&worker, &["rev-parse", "HEAD"]), source);
    assert_eq!(f.git(&worker, &["status", "--porcelain"]), "");

    for command in ["status-main", "retry-main"] {
        let again = f.publish(Path::new(PUBLISHER), &worker, &[command, "hook-composed"]);
        assert!(again.status.success(), "{command}: {}", text(&again));
        assert_eq!(f.remote_main(), main, "{command} must not push again");
    }
}

#[test]
fn missing_adjacent_policy_fails_closed_and_retry_publishes_the_pinned_source() {
    let f = Fixture::new();
    let worker = f.clone_worker("worker");
    let source = f.commit(&worker, "worker.txt", "worker change");
    f.advance_main_elsewhere("other");
    let before = f.remote_main();

    let lone = f.root.join("lone");
    std::fs::create_dir(&lone).unwrap();
    let copy = lone.join(Path::new(PUBLISHER).file_name().unwrap());
    std::fs::copy(PUBLISHER, &copy).unwrap();
    let refused = f.publish(&copy, &worker, &["submit", "hook-retry"]);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert_eq!(f.remote_main(), before, "a declining hook must leave main");
    assert!(!f.contains(&source, &f.remote_main()));

    // A newer HEAD must not be selected silently by the retry.
    f.commit(&worker, "later.txt", "later unsubmitted change");
    let retried = f.publish(Path::new(PUBLISHER), &worker, &["retry-main", "hook-retry"]);
    assert!(retried.status.success(), "{}", text(&retried));
    let main = f.remote_main();
    assert!(f.contains(&source, &main) && f.contains(&before, &main));
    let later = f.git(&worker, &["rev-parse", "HEAD"]);
    assert!(!f.contains(&later, &main));
}

#[test]
fn snapshot_tag_passes_the_real_hook_and_is_adopted_by_a_fresh_clone() {
    let f = Fixture::new();
    let worker = f.clone_worker("worker");
    let main = f.remote_main();
    let cut = f.publish(Path::new(PUBLISHER), &worker, &["cut", "hook-rc1"]);
    assert!(cut.status.success(), "{}", text(&cut));
    assert_eq!(
        f.git(
            &f.remote,
            &["rev-parse", "refs/tags/susi-snapshot/hook-rc1"]
        ),
        main
    );

    let fresh = f.clone_worker("fresh");
    let adopt = f.publish(Path::new(PUBLISHER), &fresh, &["adopt", "hook-rc1"]);
    assert!(adopt.status.success(), "{}", text(&adopt));
    assert_eq!(
        f.git(&fresh, &["rev-parse", "refs/heads/release/hook-rc1"]),
        main
    );
}
