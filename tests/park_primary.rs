#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi workflow park-primary` (`scripts/park-primary.sh` is its stub): the
//! primary checkout is parked on `main` at origin/main, detached there only
//! when another worktree holds `main`, and left alone (with the reason) when
//! it holds real work. The park was bash and is Rust
//! (`crates/susi-gawd/src/admin/park.rs`); these run it through the stub,
//! as the hooks and the loop scripts do, and through the binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str]) -> (i32, String, String) {
    let mut command = if program.ends_with(".sh") {
        #[cfg(windows)]
        {
            let mut command = Command::new("sh");
            command.arg(program);
            command
        }
        #[cfg(not(windows))]
        {
            Command::new(program)
        }
    } else {
        Command::new(program)
    };
    let out = command
        .args(args)
        .current_dir(dir)
        // The script is the stub that execs `susi workflow park-primary`:
        // the binary under test, not whatever susi the host has installed.
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
        .env_remove("SUSI_PRIMARY_SYNC_ACTIVE")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
}

impl World {
    /// bare server; primary on a stale branch one commit behind origin/main.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-park-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("primary");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&primary).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git(
            &primary,
            &["commit", "--allow-empty", "--quiet", "-m", "one"],
        );
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["switch", "--quiet", "-c", "stale-branch"]);
        // origin/main moves on.
        let other = root.join("other");
        git(
            &root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        git(&other, &["commit", "--allow-empty", "--quiet", "-m", "two"]);
        git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        Self { root, primary }
    }

    fn park(&self, from: &Path) -> (i32, String, String) {
        self.park_with(from, &[])
    }

    fn park_with(&self, from: &Path, args: &[&str]) -> (i32, String, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/park-primary.sh");
        run(from, script.to_str().unwrap(), args)
    }

    fn head_is_detached_at_origin_main(&self) -> bool {
        let detached = run(&self.primary, "git", &["symbolic-ref", "-q", "HEAD"]).0 != 0;
        detached
            && git(&self.primary, &["rev-parse", "HEAD"])
                == git(&self.primary, &["rev-parse", "origin/main"])
    }

    fn on_main_at_origin_main(&self) -> bool {
        git(&self.primary, &["symbolic-ref", "--short", "HEAD"]) == "main"
            && git(&self.primary, &["rev-parse", "HEAD"])
                == git(&self.primary, &["rev-parse", "origin/main"])
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn main_is_where_a_clean_stale_primary_is_parked() {
    let w = World::new("clean");
    let (code, out, err) = w.park(&w.primary);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("moved from stale-branch to main at origin/main"),
        "{out}"
    );
    // On the main branch, fast-forwarded (local main was one commit behind).
    assert!(w.on_main_at_origin_main());
    // Idempotent: a human's `git switch main` is never undone.
    let (code, out, _) = w.park(&w.primary);
    assert_eq!(code, 0);
    assert!(out.contains("already on main at origin/main"), "{out}");
    assert!(w.on_main_at_origin_main());
}

#[test]
fn it_can_be_run_from_a_linked_worktree() {
    let w = World::new("wt");
    let wt = w.root.join("wt");
    git(
        &w.primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            wt.to_str().unwrap(),
            "origin/main",
        ],
    );
    let (code, _, err) = w.park(&wt);
    assert_eq!(code, 0, "{err}");
    assert!(w.on_main_at_origin_main());
}

#[test]
fn main_held_by_another_worktree_makes_the_primary_detach_instead() {
    let w = World::new("held");
    let holder = w.root.join("holder");
    git(
        &w.primary,
        &[
            "worktree",
            "add",
            "--quiet",
            holder.to_str().unwrap(),
            "main",
        ],
    );
    let (code, out, err) = w.park(&w.primary);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("detached") && out.contains("holds main"),
        "{out}"
    );
    assert!(w.head_is_detached_at_origin_main());
    // The other worktree keeps main.
    assert_eq!(git(&holder, &["symbolic-ref", "--short", "HEAD"]), "main");
}

#[test]
fn main_with_local_commits_is_never_moved() {
    let w = World::new("localmain");
    git(&w.primary, &["switch", "--quiet", "main"]);
    git(
        &w.primary,
        &[
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "local only on main",
        ],
    );
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, _, err) = w.park(&w.primary);
    assert_eq!(code, 1);
    assert!(err.contains("not in origin/main"), "{err}");
    assert_eq!(
        git(&w.primary, &["rev-parse", "HEAD"]),
        before,
        "work must not move"
    );
}

#[test]
fn uncommitted_work_blocks_parking_and_is_untouched() {
    let w = World::new("dirty");
    std::fs::write(w.primary.join("wip.txt"), "unsaved").unwrap();
    let (code, _, err) = w.park(&w.primary);
    assert_eq!(code, 1);
    assert!(
        err.contains("uncommitted changes") && err.contains("wip.txt"),
        "{err}"
    );
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "stale-branch"
    );
    assert!(w.primary.join("wip.txt").exists());
}

#[test]
fn commits_not_in_origin_main_block_parking() {
    let w = World::new("unmerged");
    git(
        &w.primary,
        &["commit", "--allow-empty", "--quiet", "-m", "local only"],
    );
    let (code, _, err) = w.park(&w.primary);
    assert_eq!(code, 1);
    assert!(
        err.contains("1 commit(s) on stale-branch are not in origin/main"),
        "{err}"
    );
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "stale-branch"
    );
}

#[test]
fn sync_only_fast_forwards_a_clean_main_and_says_so() {
    let w = World::new("sync-ff");
    git(&w.primary, &["switch", "--quiet", "main"]); // one commit behind origin/main
    let (code, out, err) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("fast-forwarded to origin/main"), "{out}");
    assert!(w.on_main_at_origin_main());
    // Already current: silent no-op.
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!((code, out.trim()), (0, ""));
}

#[test]
fn sync_only_never_switches_branches_or_touches_work() {
    let w = World::new("sync-safe");
    // On a (stale) feature branch: must stay there, silently.
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!((code, out.trim()), (0, ""));
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "stale-branch"
    );

    // On main with uncommitted changes: untouched.
    git(&w.primary, &["switch", "--quiet", "main"]);
    std::fs::write(w.primary.join("wip.txt"), "unsaved").unwrap();
    git(&w.primary, &["add", "wip.txt"]);
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!((code, out.trim()), (0, ""));
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
    assert!(w.primary.join("wip.txt").exists());

    // On main with a local commit: untouched.
    git(&w.primary, &["reset", "--quiet"]);
    std::fs::remove_file(w.primary.join("wip.txt")).unwrap();
    git(
        &w.primary,
        &["commit", "--allow-empty", "--quiet", "-m", "local"],
    );
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!((code, out.trim()), (0, ""));
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
}

#[test]
fn sync_only_is_silent_and_harmless_when_offline() {
    let w = World::new("sync-offline");
    git(&w.primary, &["switch", "--quiet", "main"]);
    git(
        &w.primary,
        &["remote", "set-url", "origin", "/nonexistent/server.git"],
    );
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!((code, out.trim()), (0, ""));
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
}

#[test]
fn sync_preserves_untracked_tools_while_advancing_main() {
    let w = World::new("untracked-tools");
    git(&w.primary, &["switch", "--quiet", "main"]);
    std::fs::create_dir_all(w.primary.join(".nova")).unwrap();
    std::fs::write(w.primary.join(".nova/config"), "local tools").unwrap();
    let (code, _, err) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!(code, 0, "{err}");
    assert!(w.on_main_at_origin_main());
    assert_eq!(
        std::fs::read_to_string(w.primary.join(".nova/config")).unwrap(),
        "local tools"
    );
}

/// How long the watcher gets to observe a remote change. Generous on purpose:
/// the gate runs hundreds of tests at once and Windows process startup plus a
/// `git fetch` can take seconds under that load, while a quiet host needs one.
/// It is still bounded, so a watcher that never runs fails instead of hangs.
const WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Poll the primary's HEAD until it reaches `expected`. Returns the watcher's
/// own log on timeout: without it, a failure says only "HEAD is not the sha",
/// which is exactly the message that made this test look like a flake.
fn wait_for_head(w: &World, expected: &str, log: &Path) -> Result<String, String> {
    let deadline = std::time::Instant::now() + WATCH_TIMEOUT;
    let mut head = git(&w.primary, &["rev-parse", "HEAD"]);
    while head != expected {
        if std::time::Instant::now() >= deadline {
            let seen = std::fs::read_to_string(log).unwrap_or_default();
            return Err(format!(
                "primary HEAD never reached {expected} within {WATCH_TIMEOUT:?} \
                 (last {head}); watcher log:\n{seen}"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        head = git(&w.primary, &["rev-parse", "HEAD"]);
    }
    Ok(head)
}

/// Stop the watcher for good. On Windows `kill` terminates the shell image the
/// test spawned but not the bash that runs the script, so every run of this
/// test used to leak a watcher that kept looping until the machine rebooted.
fn stop_watcher(child: &mut std::process::Child) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .output();
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn watcher_observes_remote_merges_without_local_git_hooks() {
    let w = World::new("watcher");
    git(&w.primary, &["switch", "--quiet", "main"]);
    let scripts = w.primary.join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    for name in ["parallel-workflow.sh", "park-primary.sh", "posix-compat.sh"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
            scripts.join(name),
        )
        .unwrap();
    }
    // Keep the watcher's own words: a timeout is only diagnosable with them.
    let log = w.root.join("watcher.log");
    let log_file = std::fs::File::create(&log).unwrap();
    // `sh -c exec` makes the spawned child the script's interpreter itself, so
    // killing it stops the loop instead of orphaning a `bash` grandchild.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exec \"$0\" \"$@\"")
        .arg(scripts.join("parallel-workflow.sh"))
        .arg("watch")
        .current_dir(&w.primary)
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
        .env("SUSI_SYNC_INTERVAL", "1")
        .stdout(std::process::Stdio::from(log_file.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log_file))
        .spawn()
        .unwrap();
    // The watcher has no local hooks to lean on: first it must fast-forward the
    // primary to the origin/main that already exists (`other`'s "two").
    let other = w.root.join("other");
    let two = git(&other, &["rev-parse", "HEAD"]);
    let ready = wait_for_head(&w, &two, &log);
    if let Err(why) = ready {
        stop_watcher(&mut child);
        panic!("{why}");
    }
    // Then a merge lands while it watches: the same loop must observe it.
    git(
        &other,
        &[
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "third remote merge",
        ],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let expected = git(&other, &["rev-parse", "HEAD"]);
    let converged = wait_for_head(&w, &expected, &log);
    stop_watcher(&mut child);
    if let Err(why) = converged {
        panic!("{why}");
    }
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), expected);
}

/// The mkdir lock fallback (scripts/posix-compat.sh, used where flock(1) is
/// missing, as on Git Bash) reclaims a lock directory a killed locker left
/// empty. One left between mkdir and writing its pid held the primary-update
/// lock for a day, and every new worktree waited on it.
#[test]
fn flock_fallback_reclaims_an_empty_lock_left_by_a_killed_locker() {
    let dir = std::env::temp_dir().join(format!("susi-flock-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lock.flock.d")).unwrap();
    let compat = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/posix-compat.sh");
    // The shim is sourced by bash scripts (`set -o pipefail` is bash's; a
    // host whose `sh` is dash refuses it), so the probe runs under bash.
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            "set -euo pipefail; . \"$1\"; touch -d '1 minute ago' lock.flock.d
             exec 7>lock; flock 7; echo locked",
        )
        .arg("sh")
        .arg(compat.to_string_lossy().replace('\\', "/"))
        .current_dir(&dir)
        .env("SUSI_FLOCK_SHIM", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "locked",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `susi workflow park-primary` run as the binary, not through the stub: the
/// same verdicts and the same words, so a hook or script that execs it gets
/// exactly what the bash version said.
fn susi(dir: &Path, args: &[&str]) -> (i32, String, String) {
    run(dir, env!("CARGO_BIN_EXE_susi"), args)
}

#[test]
fn the_binary_parks_and_refuses_as_the_script_did() {
    let w = World::new("binary");
    // Dirty: refused, with the file named.
    std::fs::write(w.primary.join("wip.txt"), "unsaved").unwrap();
    let (code, _, err) = susi(&w.primary, &["workflow", "park-primary"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("uncommitted changes") && err.contains("wip.txt"),
        "{err}"
    );
    std::fs::remove_file(w.primary.join("wip.txt")).unwrap();
    // Clean and stale: parked on main, from a linked worktree too.
    let wt = w.root.join("wt");
    git(
        &w.primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            wt.to_str().unwrap(),
            "origin/main",
        ],
    );
    let (code, out, err) = susi(&wt, &["workflow", "park-primary"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("moved from stale-branch to main at origin/main"),
        "{out}"
    );
    assert!(w.on_main_at_origin_main());
    let (code, out, _) = susi(&w.primary, &["workflow", "park-primary"]);
    assert_eq!(code, 0);
    assert!(out.contains("already on main at origin/main"), "{out}");
    // --sync-only: silent when current, said when it fast-forwards.
    let (code, out, _) = susi(&w.primary, &["workflow", "park-primary", "--sync-only"]);
    assert_eq!((code, out.as_str()), (0, ""));
    let other = w.root.join("other");
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "three"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let (code, out, err) = susi(&w.primary, &["workflow", "park-primary", "--sync-only"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("fast-forwarded to origin/main"), "{out}");
    assert!(w.on_main_at_origin_main());
    // Under an active sync (a hook the fast-forward fired) nothing runs.
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "four"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["workflow", "park-primary", "--sync-only"])
        .current_dir(&w.primary)
        .env("SUSI_PRIMARY_SYNC_ACTIVE", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
}

/// A lock another sync holds: `--sync-only` leaves at once (that sync is
/// already bringing main current) and a park waits for it. A lock whose
/// holder is gone is taken over.
#[test]
fn a_sync_never_queues_behind_another_and_a_dead_lock_is_taken_over() {
    let w = World::new("lock");
    git(&w.primary, &["switch", "--quiet", "main"]); // one behind origin/main
    let common = PathBuf::from(git(
        &w.primary,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let lock = common.join("susi-primary-update.lockdir");
    std::fs::create_dir_all(&lock).unwrap();
    // This test process holds it: a live holder in this table.
    std::fs::write(
        lock.join("owner"),
        format!(
            "{} {} {} {}\n",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            if cfg!(windows) {
                std::process::id().to_string()
            } else {
                "-".to_string()
            },
            susi_gawd::admin::seed::runtime_name()
        ),
    )
    .unwrap();
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, out, _) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!(
        (code, out.as_str()),
        (0, ""),
        "a held lock: silent, no wait"
    );
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
    assert!(lock.is_dir(), "another sync's lock is never removed");
    // The holder is gone (a pid nothing runs under): taken over, main synced.
    std::fs::write(
        lock.join("owner"),
        format!(
            "4000000000 1 - {}\n",
            susi_gawd::admin::seed::runtime_name()
        ),
    )
    .unwrap();
    let (code, out, err) = w.park_with(&w.primary, &["--sync-only"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("fast-forwarded to origin/main"), "{out}");
    assert!(!lock.exists(), "the lock is let go of after");
}

/// On Windows an installed `susi.exe` parks the primary with no Git userland
/// on PATH: only Git's `cmd` (git itself), System32 and the Windows
/// directory. The bash version needed `flock`, `awk` and `dirname` from
/// Git's `usr/bin`. Returns at once where Git for Windows is not installed.
#[test]
fn the_park_needs_no_git_userland_on_windows() {
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
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["workflow", "park-primary"])
        .current_dir(&w.primary)
        .env("PATH", &path)
        .env_remove("SUSI_PRIMARY_SYNC_ACTIVE")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("moved from stale-branch to main at origin/main"),
        "{said}"
    );
    assert!(w.on_main_at_origin_main());
}
