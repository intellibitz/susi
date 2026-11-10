//! The built `local-snapshot` publisher composed with the built `push-policy`
//! helper, through the repository's own `.githooks/pre-push`, against real Git.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

const PUBLISHER: &str = env!("CARGO_BIN_EXE_local-snapshot");
const POLICY: &str = env!("CARGO_BIN_EXE_push-policy");

// Every test launches the same native publisher and policy helpers. Keep the
// fixture processes apart: on Linux, a sibling test can otherwise try to
// exec a copied helper while another thread still has that path open for
// replacement, yielding ETXTBSY instead of a meaningful test result.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The inherited search path without the build directory, so the hook can only
/// find `push-policy` through the publisher's adjacent-helper discovery.
fn path_without_helper() -> OsString {
    let helper_dir = Path::new(POLICY).parent().unwrap();
    let helper_name = Path::new(POLICY).file_name().unwrap();
    let kept: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p != helper_dir && !p.join(helper_name).exists())
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

#[path = "../src/test_fixture_fs.rs"]
mod test_fixture_fs;
use test_fixture_fs::copy_dir_all;

struct Fixture {
    root: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static FX_TPL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let tpl = FX_TPL.get_or_init(|| {
            let tpl_root =
                std::env::temp_dir().join(format!("susi-pubhook-tpl-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tpl_root);
            std::fs::create_dir_all(&tpl_root).unwrap();
            let hooks = tpl_root.join("hooks");
            std::fs::create_dir(&hooks).unwrap();
            let hook = hooks.join("pre-push");
            std::fs::write(&hook, include_str!("../../.githooks/pre-push")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let remote = tpl_root.join("remote.git");
            let f = Self {
                root: tpl_root.clone(),
                remote,
            };
            f.git(
                &f.root,
                &["init", "-q", "--bare", "-b", "main", "remote.git"],
            );
            let seed = f.clone_worker("seed");
            f.commit(&seed, "seed.txt", "seed");
            f.push_main_with_policy(&seed);
            let _ = std::fs::remove_dir_all(&seed);
            std::mem::forget(f);
            tpl_root
        });

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi-publisher-hook-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        copy_dir_all(tpl, &root);
        let remote = root.join("remote.git");
        Self { root, remote }
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
        let hooks = self.root.join("hooks");
        let hooks_cfg = format!("core.hooksPath={}", hooks.to_str().unwrap());
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                "--no-hardlinks",
                "-c",
                &hooks_cfg,
                "-c",
                "commit.gpgsign=false",
                remote,
                name,
            ],
        );
        let dir = self.root.join(name);
        if !dir.join(".git/refs/heads/main").exists()
            && self.git(&dir, &["branch", "--show-current"]).is_empty()
        {
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
    let _lock = test_lock();
    assert_eq!(Path::new(PUBLISHER).parent(), Path::new(POLICY).parent());
}

#[test]
fn submit_composes_divergent_main_through_the_real_hook_and_adjacent_policy() {
    let _lock = test_lock();
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
    let _lock = test_lock();
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
    let _lock = test_lock();
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
    let remote_leaked = isolated(&mut Command::new("git"), &f.remote)
        .args(["show-ref", "--verify", "refs/heads/release/hook-rc1"])
        .status()
        .unwrap();
    assert!(!remote_leaked.success());
}

#[test]
fn local_distill_publish_passes_the_real_hook_and_adjacent_policy_and_stays_immutable() {
    let _lock = test_lock();
    const DISTILL: &str = env!("CARGO_BIN_EXE_local-distill");
    assert_eq!(Path::new(DISTILL).parent(), Path::new(POLICY).parent());

    let f = Fixture::new();
    let worker = f.clone_worker("distill-worker");
    let base = f.remote_main();

    std::fs::create_dir_all(worker.join(".agents/tasks")).unwrap();
    std::fs::write(
        worker.join(".agents/tasks/T-HOOKDISTILL-1.json"),
        r#"{
  "id": "T-HOOKDISTILL-1",
  "title": "Distill candidate hook fixture",
  "goal": "Prove local-distill publish traverses .githooks/pre-push and push-policy",
  "created_by": "fixture",
  "created_unix": 1794350000,
  "status": "done",
  "deps": [],
  "scopes": [".agents/tasks/T-HOOKDISTILL-1.json", "distilled.txt"],
  "accept": { "cmd": ["cargo", "--version"] }
}
"#,
    )
    .unwrap();
    std::fs::write(worker.join("distilled.txt"), "distilled payload\n").unwrap();
    f.git(
        &worker,
        &["add", ".agents/tasks/T-HOOKDISTILL-1.json", "distilled.txt"],
    );
    f.git(
        &worker,
        &[
            "commit",
            "-q",
            "-m",
            "feat: distilled change\n\nTask: T-HOOKDISTILL-1",
        ],
    );

    let compose = f.publish(
        Path::new(DISTILL),
        &worker,
        &[
            "compose",
            "--name",
            "hook-distill-rc1",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-HOOKDISTILL-1",
            "--strict",
            "--json",
        ],
    );
    assert!(compose.status.success(), "{}", text(&compose));
    let receipt: serde_json::Value = serde_json::from_slice(&compose.stdout).unwrap();
    let expected_commit = receipt["final_commit"].as_str().unwrap().to_owned();

    let published = f.publish(
        Path::new(DISTILL),
        &worker,
        &["publish", "hook-distill-rc1", "--json"],
    );
    assert!(published.status.success(), "{}", text(&published));
    assert_eq!(
        f.git(
            &f.remote,
            &["rev-parse", "refs/tags/susi-distill/hook-distill-rc1"]
        ),
        expected_commit
    );

    let status = f.publish(
        Path::new(DISTILL),
        &worker,
        &["status", "hook-distill-rc1", "--json"],
    );
    assert!(status.status.success(), "{}", text(&status));
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["remote_state"], "published_exact");

    // Attempting to overwrite or delete the published distill tag through the real hook fails closed.
    let mut path = vec![Path::new(POLICY).parent().unwrap().to_owned()];
    path.extend(std::env::split_paths(&path_without_helper()));
    let joined_path = std::env::join_paths(path).unwrap();
    for bad_spec in [
        format!("{base}:refs/tags/susi-distill/hook-distill-rc1"),
        ":refs/tags/susi-distill/hook-distill-rc1".to_owned(),
    ] {
        let refusal = isolated(&mut Command::new("git"), &worker)
            .env("PATH", &joined_path)
            .args(["push", "--force", "origin", &bad_spec])
            .output()
            .unwrap();
        assert!(!refusal.status.success(), "{}", text(&refusal));
    }
    assert_eq!(
        f.git(
            &f.remote,
            &["rev-parse", "refs/tags/susi-distill/hook-distill-rc1"]
        ),
        expected_commit
    );

    // Orchestrating an accept-verified release candidate creates only a local
    // release ref. Release branches are not a GitHub merge surface.
    let rel_orch = f.publish(
        Path::new(DISTILL),
        &worker,
        &[
            "orchestrate",
            "--name",
            "v0.21.55-hook-rc1",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-HOOKDISTILL-1",
            "--verify-accept",
            "--release-candidate",
            "--strict",
        ],
    );
    assert!(rel_orch.status.success(), "{}", text(&rel_orch));
    let local_rel = f.git(
        &worker,
        &["rev-parse", "refs/heads/release/v0.21.55-hook-rc1"],
    );
    assert!(!local_rel.is_empty());
    let remote_rel = isolated(&mut Command::new("git"), &f.remote)
        .args([
            "show-ref",
            "--verify",
            "refs/heads/release/v0.21.55-hook-rc1",
        ])
        .status()
        .unwrap();
    assert!(!remote_rel.success(), "release candidate leaked to remote");

    let publish_branch = f.publish(
        Path::new(DISTILL),
        &worker,
        &["publish-branch", "v0.21.55-hook-rc1"],
    );
    assert!(
        !publish_branch.status.success(),
        "{}",
        text(&publish_branch)
    );
    assert!(
        text(&publish_branch).contains("local-only"),
        "{}",
        text(&publish_branch)
    );

    let rel_status = f.publish(
        Path::new(DISTILL),
        &worker,
        &["status", "v0.21.55-hook-rc1", "--json"],
    );
    assert!(rel_status.status.success(), "{}", text(&rel_status));
    let rel_status_json: serde_json::Value = serde_json::from_slice(&rel_status.stdout).unwrap();
    assert_eq!(rel_status_json["branch_state"], "present_exact");
    assert_eq!(rel_status_json["remote_branch_state"], "local_only");
}

#[test]
fn every_submit_witnesses_the_remote_and_reports_lost_history_without_blocking() {
    let _lock = test_lock();
    let f = Fixture::new();
    let seed = f.remote_main();
    let worker = f.clone_worker("worker");
    f.commit(&worker, "worker.txt", "first");
    let other = f.advance_main_elsewhere("other");
    let out = f.publish(Path::new(PUBLISHER), &worker, &["submit", "witness-one"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("storage guard"));
    let record = "refs/susi-guard/origin/heads/main";
    let witnessed = f.remote_main();
    assert_eq!(f.git(&worker, &["rev-parse", record]), witnessed);

    // Another client with no hook rewinds main: the other agent's commit and
    // the merge are gone, and this worker's own branch never contained them.
    f.git(&f.remote, &["update-ref", "refs/heads/main", &seed]);
    let second = f.commit(&worker, "worker.txt", "second");
    let out = f.publish(Path::new(PUBLISHER), &worker, &["submit", "witness-two"]);
    // The dump is never blocked; the loss is reported and the record is kept.
    assert!(out.status.success(), "{}", text(&out));
    assert!(f.contains(&second, &f.remote_main()));
    assert!(!f.contains(&other, &f.remote_main()));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("REWOUND refs/heads/main"), "{}", text(&out));
    assert!(stderr.contains("did not block"), "{}", text(&out));
    assert_eq!(f.git(&worker, &["rev-parse", record]), witnessed);
    // What was lost is still in the witnessing clone.
    assert!(isolated(&mut Command::new("git"), &worker)
        .args(["merge-base", "--is-ancestor", &other, record])
        .status()
        .unwrap()
        .success());

    let quiet = isolated(&mut Command::new(PUBLISHER), &worker)
        .env("SUSI_NO_STORAGE_GUARD", "1")
        .args(["retry-main", "witness-two"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("storage guard"));
}

/// `local-workflow ship` is the gated landing of Susi core and, through its
/// cargo alias, of Susi Commercial Apps. Whatever the ship's result, the
/// shipping clone witnesses the storage remote afterwards.
#[test]
fn a_ship_witnesses_the_remote_whatever_its_result() {
    let _lock = test_lock();
    let f = Fixture::new();
    let worker = f.clone_worker("worker");
    let record = "refs/susi-guard/origin/heads/main";
    let ship = |extra: &[(&str, &str)]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_local-workflow"));
        isolated(&mut command, &worker).args(["ship", "no-such-reservation"]);
        for (key, value) in extra {
            command.env(key, value);
        }
        command.output().unwrap()
    };
    let skipped = ship(&[("SUSI_NO_STORAGE_GUARD", "1")]);
    assert!(!skipped.status.success(), "{}", text(&skipped));
    assert!(isolated(&mut Command::new("git"), &worker)
        .args(["rev-parse", "-q", "--verify", record])
        .output()
        .unwrap()
        .stdout
        .is_empty());

    let out = ship(&[]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("storage guard"));
    assert_eq!(f.git(&worker, &["rev-parse", record]), f.remote_main());

    // History dropped behind this clone's back is reported by the next ship.
    let lost = f.advance_main_elsewhere("other");
    ship(&[]);
    f.git(
        &f.remote,
        &["update-ref", "refs/heads/main", &format!("{lost}^")],
    );
    let out = ship(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("REWOUND refs/heads/main"), "{}", text(&out));
}
