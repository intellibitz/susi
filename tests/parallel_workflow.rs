// These fixtures are POSIX shell: the fake `cargo`/`git`/`gh` tools are
// `#!/bin/sh` scripts, `sh` runs `scripts/*.sh`, and the fixture relies on
// `chmod` and symlinks. A native Windows host cannot reproduce that process
// model, and the file has stalled the root-package gate there for 16+ minutes,
// so Windows skips it. Unix (Linux, macOS and CI) still runs every test.
#![allow(missing_docs)] // integration test crate (and an empty crate on Windows)
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test fixtures/assertions
//! Exercise finish against a local bare remote, simulating its merge automation.

use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.arg(script);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(script)
    }
}

fn path_with_prefix(prefix: &Path) -> std::ffi::OsString {
    let mut paths = vec![prefix.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).unwrap()
}

fn host_git_binary() -> String {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            #[cfg(windows)]
            let candidate = dir.join("git.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("git");
            if candidate.is_file() {
                return candidate.to_string_lossy().replace('\\', "/");
            }
        }
    }
    if cfg!(windows) {
        "git.exe".to_string()
    } else {
        "/usr/bin/git".to_string()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct World {
    root: PathBuf,
    primary: PathBuf,
    work: PathBuf,
    remote: PathBuf,
    bins: PathBuf,
}
impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-finish-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let primary = root.join("primary");
        let work = root.join("worker");
        let remote = root.join("remote.git");
        let bins = root.join("bins");
        for dir in [
            &primary,
            &remote,
            &bins,
            &root.join("home"),
            &root.join("cargo-home"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.name", "test"]);
        git(&primary, &["config", "user.email", "test@example.test"]);
        git(&primary, &["config", "commit.gpgsign", "false"]);
        executable(
            &primary.join(".git/hooks/pre-push"),
            "#!/bin/sh\nwhile read local_ref local_sha remote_ref remote_sha; do\n case \"$remote_ref\" in\n  refs/heads/worker)\n   if [ -n \"$TEST_RACE_PRIMARY\" ] && [ ! -f \"$TEST_RACE_USED\" ]; then\n    : > \"$TEST_RACE_USED\"\n    git -C \"$TEST_RACE_PRIMARY\" push -q origin main\n   elif [ \"$TEST_NO_MERGE\" != 1 ]; then\n    git -C \"$TEST_REMOTE\" update-ref refs/heads/main \"$local_sha\"\n   fi ;;\n esac\ndone\n",
        );
        git(
            &primary,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let scripts = primary.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        for name in [
            "parallel-workflow.sh",
            "park-primary.sh",
            "ensure-watcher.sh",
            "swarm-status.sh",
            "check-owed-runs.sh",
            "ci-changed-crates.sh",
            "check-hermetic-tests.sh",
            "posix-compat.sh",
        ] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(name),
                scripts.join(name),
            )
            .unwrap();
        }
        // A fresh heartbeat, so `sync`'s watcher heal is a no-op here: these
        // tests must never spawn a background watcher into the temp fixture.
        std::fs::write(
            primary.join(".git/susi-primary-watch.stamp"),
            format!("{}\n", now_unix()),
        )
        .unwrap();
        executable(&scripts.join("accept.sh"), "#!/bin/sh\nexit 0\n");
        // A minimal workspace, so the gate's crate scoping has metadata to
        // read: one member crate plus a root package owning src/ and tests/.
        std::fs::write(
            primary.join("Cargo.toml"),
            "[package]\nname = \"fixture-root\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\ncell = { path = \"crates/cell\" }\n\n[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(primary.join("src")).unwrap();
        std::fs::write(primary.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        std::fs::create_dir_all(primary.join("crates/cell/src")).unwrap();
        std::fs::write(
            primary.join("crates/cell/Cargo.toml"),
            "[package]\nname = \"cell\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(primary.join("crates/cell/src/lib.rs"), "pub fn cell() {}\n").unwrap();
        std::fs::create_dir_all(primary.join(".agents/tasks")).unwrap();
        std::fs::write(
            primary.join(".agents/tasks/T-WORKER-1.json"),
            serde_json::json!({
                "id":"T-WORKER-1", "title":"marker", "goal":"marker", "size":"s",
                "accept":{"cmd":["scripts/accept.sh"]}, "created_by":"WORKER", "created_unix":0
            })
            .to_string(),
        )
        .unwrap();
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "-qm", "seed"]);
        git(&primary, &["push", "-q", "origin", "main"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "worker",
                work.to_str().unwrap(),
                "origin/main",
            ],
        );
        // The fake records its full argv so a test can see which packages the
        // gate selected; `metadata` delegates to the real toolchain because
        // `ci-changed-crates.sh` reads an actual workspace.
        executable(&bins.join("cargo"), "#!/bin/sh\nif [ \"$1\" = metadata ]; then exec \"$TEST_REAL_CARGO\" \"$@\"; fi\necho \"$*\" >> \"$TEST_GATES\"\nif [ \"$1 $2\" = \"nextest --version\" ] && [ \"$TEST_NO_NEXTEST\" = 1 ]; then exit 1; fi\nif [ \"$1 $2\" = \"nextest run\" ]; then echo \"HOME=$HOME FORBIDDEN=$SUSI_HERMETIC_FORBIDDEN\" >> \"$TEST_GATES.env\"; fi\nif [ \"$1\" = clippy ] && [ \"$TEST_GATE_FAIL\" = 1 ]; then exit 1; fi\n");
        // The hermetic wrapper puts `$CARGO_HOME/bin` ahead of PATH, so the fake
        // has to be reachable there too.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&bins, root.join("cargo-home").join("bin")).unwrap();
        // GitHub's successful PR merge is emulated only for the code branch push.
        // With TEST_RACE_PRIMARY set, the first push instead lands the primary's
        // unpushed main commit on remote main — a merge that raced ours. Each
        // branch push is recorded in TEST_PUSHES when it is set.
        executable(
            &bins.join("git"),
            &format!(
                "#!/bin/sh\n\"{g}\" \"$@\" || exit $?\nif [ \"$1\" = push ]; then\n for arg in \"$@\"; do\n  case \"$arg\" in HEAD:refs/heads/*)\n   if [ -n \"$TEST_PUSHES\" ]; then echo \"$arg\" >> \"$TEST_PUSHES\"; fi\n   if [ -n \"$TEST_RACE_PRIMARY\" ] && [ ! -f \"$TEST_RACE_USED\" ]; then\n    : > \"$TEST_RACE_USED\"\n    (cd \"$TEST_RACE_PRIMARY\" && \"{g}\" push -q origin main)\n   elif [ \"$TEST_NO_MERGE\" != 1 ]; then\n    sha=$(\"{g}\" rev-parse HEAD); \"{g}\" --git-dir=\"$TEST_REMOTE\" update-ref refs/heads/main \"$sha\"\n   fi ;;\n  esac\n done\nfi\n",
                g = host_git_binary()
            ),
        );
        let world = Self {
            root,
            primary,
            work,
            remote,
            bins,
        };
        let out = world
            .susi(&["tasks", "claim", "T-WORKER-1", "--scope", "code.txt"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::write(world.work.join("code.txt"), "implemented").unwrap();
        git(&world.work, &["add", "code.txt"]);
        git(
            &world.work,
            &["commit", "-qm", "Implementation\n\nTask: T-WORKER-1"],
        );
        world
    }
    fn susi(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
        susi_paths::test_env::isolate_home(&mut cmd, &self.root.join("home"));
        cmd.args(args)
            .current_dir(&self.work)
            .env("CARGO_HOME", self.root.join("cargo-home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/xdg"))
            // The gate reads these: a developer who exports them must not change
            // what the fixture's gate runs.
            .env_remove("SUSI_HERMETIC_RUNNER")
            .env_remove("SUSI_HERMETIC_RETRIES")
            .env_remove("SUSI_LOCAL_GATE")
            // So does the cargo it runs: the fixture's builds stay out of the
            // caller's target.
            .env_remove("CARGO_TARGET_DIR")
            .env("SUSI_AGENT", "WORKER");
        cmd
    }
    fn finish(&self, fail: bool) -> std::process::Output {
        self.finish_env(fail, &[])
    }

    fn finish_env(&self, fail: bool, envs: &[(&str, &str)]) -> std::process::Output {
        let mut command = self.susi(&["workflow", "finish", "T-WORKER-1"]);
        command
            .env("PATH", path_with_prefix(&self.bins))
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", if fail { "1" } else { "0" })
            // A gate failure needs a gate that runs: the fixture's work
            // touches no crate, so the scoped gate skips clippy entirely.
            .env("SUSI_LOCAL_GATE", if fail { "full" } else { "scoped" })
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .envs(envs.iter().copied());
        #[cfg(windows)]
        {
            let no_merge = envs
                .iter()
                .any(|(key, value)| *key == "TEST_NO_MERGE" && *value == "1");
            let initial_branch = Command::new("git")
                .args(["--git-dir"])
                .arg(&self.remote)
                .args(["rev-parse", "--verify", "refs/heads/worker"])
                .output()
                .unwrap();
            let initial_branch = initial_branch.status.success().then(|| {
                String::from_utf8_lossy(&initial_branch.stdout)
                    .trim()
                    .to_owned()
            });
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            if !no_merge {
                for _ in 0..1_500 {
                    if child.try_wait().unwrap().is_some() {
                        break;
                    }
                    let pushed = Command::new("git")
                        .args(["--git-dir"])
                        .arg(&self.remote)
                        .args(["rev-parse", "--verify", "refs/heads/worker"])
                        .output()
                        .unwrap();
                    if pushed.status.success() {
                        let sha = String::from_utf8_lossy(&pushed.stdout).trim().to_owned();
                        if initial_branch.as_deref() == Some(sha.as_str()) {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            continue;
                        }
                        let merged = Command::new("git")
                            .args(["--git-dir"])
                            .arg(&self.remote)
                            .args(["update-ref", "refs/heads/main", &sha])
                            .status()
                            .unwrap();
                        assert!(merged.success(), "fixture merge update failed");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            child.wait_with_output().unwrap()
        }
        #[cfg(not(windows))]
        {
            command.output().unwrap()
        }
    }

    /// `finish` while another merge lands on main exactly as our branch
    /// pushes: the head is now behind main but merges cleanly, which is the
    /// merge machinery's to gate, so the wait loop neither re-runs the gate nor
    /// repushes (a repush cancelled the branch's own in-flight run).
    fn finish_racing_an_intervening_merge(&self) -> std::process::Output {
        // The competitor's commit lives on the primary's main but is unpushed;
        // the fake remote lands it when our branch push arrives.
        git(
            &self.primary,
            &["commit", "--allow-empty", "-qm", "someone else merged"],
        );
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env("PATH", path_with_prefix(&self.bins))
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_RACE_PRIMARY", &self.primary)
            .env("TEST_RACE_USED", self.root.join("race-used"))
            .env("TEST_PUSHES", self.root.join("pushes"))
            .env("SUSI_FINISH_WAIT", "1")
            .env("SUSI_FINISH_WAIT_MAX", "3")
            .env("SUSI_FINISH_POLL", "1")
            .output()
            .unwrap()
    }

    /// Rewrite the task's claim with a lease that already lapsed, as if the
    /// agent had been away (a suspend, a very long gate) past its 4h lease.
    fn expire_claim(&self) {
        let live = git(&self.remote, &["rev-parse", "refs/claims/T-WORKER-1"]);
        let body = git(&self.remote, &["cat-file", "-p", &live]);
        let mut claim: serde_json::Value = serde_json::from_str(&body).unwrap();
        claim["lease_until_unix"] = serde_json::json!(1);
        let path = self.root.join("expired-claim.json");
        std::fs::write(&path, serde_json::to_string(&claim).unwrap()).unwrap();
        let blob = git(&self.remote, &["hash-object", "-w", path.to_str().unwrap()]);
        git(
            &self.remote,
            &["update-ref", "refs/claims/T-WORKER-1", &blob],
        );
        // A claim is one blob pushed to three refs, so the agent ref lapses with
        // the task ref. Leaving it live makes the CLI correctly refuse the
        // re-claim as "already holds", which is a different situation.
        let agent = git(
            &self.remote,
            &["for-each-ref", "--format=%(refname)", "refs/claim-agents/"],
        );
        for r in agent.lines() {
            git(&self.remote, &["update-ref", r, &blob]);
        }
    }

    /// `finish --wait` on a branch whose push never becomes a merge, with a
    /// one-second poll and a three-second budget so the wait is observable in
    /// a test. (Waiting is opt-in: the `--wait` flag here.)
    fn finish_without_merging(&self) -> std::process::Output {
        // The flag, through the binary: the documented way to opt in.
        self.susi(&["workflow", "finish", "T-WORKER-1", "--wait"])
            .env("PATH", path_with_prefix(&self.bins))
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_NO_MERGE", "1")
            .env("SUSI_FINISH_POLL", "1")
            .env("SUSI_FINISH_WAIT_MAX", "3")
            .output()
            .unwrap()
    }

    /// `finish` where the branch never merges but `gh` reports the run for the
    /// pushed sha green. The budget dwarfs the run-check cadence, so passing
    /// proves the loop handed off on green rather than waiting out the merge.
    fn finish_with_a_green_run(&self) -> std::process::Output {
        executable(
            &self.bins.join("gh"),
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n\"run list\")\n sha=$(\"{}\" rev-parse HEAD 2>/dev/null)\n printf '[{{\"headSha\":\"%s\",\"status\":\"completed\",\"conclusion\":\"success\"}}]\\n' \"$sha\"\n ;;\n\"pr view\") echo OPEN ;;\n*) exit 1 ;;\nesac\n",
                host_git_binary()
            ),
        );
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env("PATH", path_with_prefix(&self.bins))
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_NO_MERGE", "1")
            .env("SUSI_FINISH_WAIT", "1")
            .env("SUSI_FINISH_POLL", "1")
            .env("SUSI_FINISH_WAIT_MAX", "8")
            .output()
            .unwrap()
    }

    /// `finish` where the branch never merges *and* `gh` reports that the run
    /// for the pushed sha has failed. The budget is larger than the run-check
    /// cadence, so a passing test proves the loop stopped on the failure rather
    /// than on the budget.
    fn finish_with_a_failing_run(&self) -> std::process::Output {
        executable(
            &self.bins.join("gh"),
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n\"run list\")\n sha=$(\"{}\" rev-parse HEAD 2>/dev/null)\n printf '[{{\"headSha\":\"%s\",\"status\":\"completed\",\"conclusion\":\"failure\"}}]\\n' \"$sha\"\n ;;\n\"pr view\") echo OPEN ;;\n*) exit 1 ;;\nesac\n",
                host_git_binary()
            ),
        );
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env("PATH", path_with_prefix(&self.bins))
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_NO_MERGE", "1")
            .env("SUSI_FINISH_WAIT", "1")
            .env("SUSI_FINISH_POLL", "1")
            .env("SUSI_FINISH_WAIT_MAX", "8")
            .output()
            .unwrap()
    }
}
impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn finish_waits_for_publication_before_releasing_and_syncs_primary() {
    let w = World::new("success");
    let out = w.finish_env(false, &[("SUSI_FINISH_WAIT", "1")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    assert_eq!(
        git(&w.primary, &["rev-parse", "HEAD"]),
        git(&w.work, &["rev-parse", "HEAD"])
    );
    assert!(git(
        &w.work,
        &[
            "ls-remote",
            "origin",
            "refs/claims/*",
            "refs/claim-agents/*"
        ]
    )
    .is_empty());
    // The fixture's work touches only code.txt — no crate — so the scoped
    // gate proves fmt locally and leaves compile/test to the branch run.
    assert_eq!(
        std::fs::read_to_string(w.root.join("gates")).unwrap(),
        "fmt --all --check\n"
    );
}

/// The fixture's gate runs cargo; inheriting the suite's `CARGO_TARGET_DIR`,
/// it would build into the caller's target.
#[test]
fn the_fixtures_gate_never_reaches_the_callers_cargo_target() {
    let w = World::new("target");
    let command = w.susi(&["workflow", "finish", "T-WORKER-1"]);
    let removed: Vec<&std::ffi::OsStr> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key)
        .collect();
    assert!(
        removed.contains(&std::ffi::OsStr::new("CARGO_TARGET_DIR")),
        "CARGO_TARGET_DIR must not reach the fixture's gate"
    );
}

/// A diff that touches a member crate gates that crate — clippy and the tests
/// both carry `-p cell` and nothing runs `--workspace`. Before the gate was
/// scoped, this task paid for the whole workspace like every other. The tests
/// run once, under nextest, inside the hermetic wrapper.
#[test]
fn the_gate_scopes_to_the_crates_the_diff_touched() {
    let w = World::new("scoped");
    std::fs::write(w.work.join("crates/cell/src/lib.rs"), "pub fn cell2() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "touch cell\n\nTask: T-WORKER-1"],
    );
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(
        gates.contains("clippy --locked --all-targets -p cell"),
        "scoped clippy: {gates}"
    );
    assert!(
        gates.contains("nextest run --locked --no-fail-fast --no-tests=pass --retries 2 -p cell\n"),
        "scoped tests, once, under nextest with per-test retries: {gates}"
    );
    assert!(!gates.contains("--workspace"), "{gates}");
}

/// What can stop *compiling* reaches further than what can fail a test: the root
/// package depends on `cell`, so a change to `cell` lints both, but only `cell`'s
/// own tests can have changed. The pre-push hook lints this same set.
#[test]
fn the_gate_lints_the_dependents_but_tests_only_the_changed_crates() {
    let w = World::new("dependents");
    std::fs::write(w.work.join("crates/cell/src/lib.rs"), "pub fn cell2() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "touch cell\n\nTask: T-WORKER-1"],
    );
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(
        gates.contains("clippy --locked --all-targets -p cell -p fixture-root -- -D warnings\n"),
        "clippy covers the changed crate and what depends on it: {gates}"
    );
    let tests: Vec<&str> = gates
        .lines()
        .filter(|l| l.starts_with("nextest run"))
        .collect();
    assert_eq!(tests.len(), 1, "the tests run once: {gates}");
    assert!(
        !tests[0].contains("fixture-root"),
        "a dependent's tests are not what the diff changed: {}",
        tests[0]
    );
}

/// Mandate 52: the single test run is hermetic - a throwaway HOME and a
/// SUSI_HOME the wrapper then checks stays empty - not the developer's own.
#[test]
fn the_gates_test_run_is_hermetic() {
    let w = World::new("hermetic");
    std::fs::write(w.work.join("crates/cell/src/lib.rs"), "pub fn cell2() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "touch cell\n\nTask: T-WORKER-1"],
    );
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = std::fs::read_to_string(w.root.join("gates.env")).unwrap();
    let home = w.root.join("home");
    assert!(
        !env.contains(&format!("HOME={}", home.display())),
        "the tests ran with the developer's HOME: {env}"
    );
    assert!(
        env.contains("FORBIDDEN=/"),
        "the throwaway SUSI_HOME the wrapper guards was not set: {env}"
    );
}

/// A workspace-wide input (a lockfile bump, a toolchain pin, `SUSI_LOCAL_GATE=full`)
/// takes the same single hermetic nextest run as a scoped diff, over the whole
/// workspace. It used to be plain `cargo test` while a workspace-wide hermetic
/// sweep was not clean; it is now (3,675 tests, and SUSI_HOME stays empty).
#[test]
fn a_workspace_wide_gate_is_hermetic_and_runs_under_nextest_too() {
    let w = World::new("allhermetic");
    let out = w.finish_env(false, &[("SUSI_LOCAL_GATE", "full")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(
        gates.contains("clippy --workspace --all-targets --locked -- -D warnings\n"),
        "{gates}"
    );
    assert!(
        gates.contains(
            "nextest run --locked --no-fail-fast --no-tests=pass --retries 2 --workspace\n"
        ),
        "the workspace-wide run is one hermetic nextest pass: {gates}"
    );
    assert!(
        std::fs::read_to_string(w.root.join("gates.env"))
            .unwrap()
            .contains("FORBIDDEN=/"),
        "and it ran under the hermetic wrapper"
    );
}

/// A host without cargo-nextest keeps the old path: `cargo test`, retrying the
/// crates that failed once. The gate must not stop working there.
#[test]
fn without_nextest_the_gate_falls_back_to_cargo_test() {
    let w = World::new("nonextest");
    std::fs::write(w.work.join("crates/cell/src/lib.rs"), "pub fn cell2() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "touch cell\n\nTask: T-WORKER-1"],
    );
    let out = w.finish_env(false, &[("TEST_NO_NEXTEST", "1")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(gates.contains("test --locked -p cell"), "{gates}");
    assert!(!gates.contains("nextest run"), "{gates}");
}

/// Root-package trees (`tests/`, `src/`) belong to the root package, not to
/// ALL: a tests/-only change gates `-p fixture-root`, not the workspace.
#[test]
fn root_owned_paths_gate_the_root_package() {
    let w = World::new("roottests");
    std::fs::create_dir_all(w.work.join("tests")).unwrap();
    std::fs::write(w.work.join("tests/t.rs"), "#[test] fn t() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "add a root test\n\nTask: T-WORKER-1"],
    );
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(gates.contains("-p fixture-root"), "{gates}");
    assert!(!gates.contains("--workspace"), "{gates}");
}

/// The gate already ran the task's acceptance — the diff touched `cell`, so
/// the gate's `cargo test -p cell` is the same suite the `cargo test -p cell`
/// acceptance would run. `finish` marks the task covered and `close` records
/// the acceptance without paying for a second identical suite.
#[test]
fn a_cargo_acceptance_the_gate_covered_is_not_run_again() {
    let w = World::new("covered");
    std::fs::write(
        w.work.join(".agents/tasks/T-WORKER-1.json"),
        serde_json::json!({
            "id":"T-WORKER-1", "title":"marker", "goal":"marker", "size":"s",
            "accept":{"cmd":["cargo","test","-p","cell","some_test"]},
            "created_by":"WORKER", "created_unix":0
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(w.work.join("crates/cell/src/lib.rs"), "pub fn cell2() {}\n").unwrap();
    git(&w.work, &["add", "-A"]);
    git(
        &w.work,
        &["commit", "-qm", "touch cell\n\nTask: T-WORKER-1"],
    );
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert!(
        gates.contains("nextest run --locked --no-fail-fast --no-tests=pass --retries 2 -p cell\n"),
        "the gate ran cell's tests: {gates}"
    );
    assert!(
        !gates.contains("test -p cell some_test"),
        "close must not re-run the covered acceptance: {gates}"
    );
}

/// A scripts/ acceptance is not a cargo suite — the gate cannot have run it,
/// so `close` executes it even when finish passes a coverage flag for the task.
#[test]
fn a_scripts_acceptance_is_never_skipped() {
    let w = World::new("scriptaccept");
    let out = w.finish(false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The fixture acceptance is scripts/accept.sh: not cargo-shaped, so close
    // ran it regardless — visible as the task reaching done/.
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
}

/// When another merge lands between our gate and our push, the head is behind
/// main but merges cleanly. That is the merge machinery's case: it gates the
/// merge tree itself (auto-merge-pr.sh's inline gate). So finish neither
/// re-runs the gate (that multiplied the workspace suite by every raced
/// merge) nor repushes (that cancelled the branch's own in-flight run, so in a
/// busy swarm a slow branch never went green); past the wait budget it hands
/// the merge to the machinery and frees the agent.
#[test]
fn an_intervening_merge_neither_re_runs_the_gate_nor_repushes() {
    let w = World::new("race");
    let out = w.finish_racing_an_intervening_merge();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gates = std::fs::read_to_string(w.root.join("gates")).unwrap();
    assert_eq!(
        gates.matches("fmt --all --check").count(),
        1,
        "the gate ran exactly once despite the raced merge: {gates}"
    );
    let pushes = std::fs::read_to_string(w.root.join("pushes")).unwrap_or_default();
    assert_eq!(
        pushes.lines().count(),
        1,
        "the branch was pushed once; a clean behind head is not repushed: {pushes}"
    );
    // The task is closed, and the claim is handed to the merge machinery.
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    assert!(git(&w.work, &["ls-remote", "origin", "refs/claims/*"]).is_empty());
}

#[test]
fn failed_gate_leaves_task_open_claim_owned_and_remote_unchanged() {
    let w = World::new("failure");
    let before = git(&w.primary, &["rev-parse", "HEAD"]);
    assert!(!w.finish(true).status.success());
    assert!(w.work.join(".agents/tasks/T-WORKER-1.json").exists());
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), before);
    assert!(!git(&w.work, &["ls-remote", "origin", "refs/claims/*"]).is_empty());
}

/// A green head that is merely queued is the merge machinery's problem, not
/// the agent's — waiting out the whole convoy while holding the claim is how
/// agents used to sit idle through merge storms. Past the budget, `finish`
/// hands the merge off: it releases the claim (the close receipt keeps the
/// debt, so the work cannot be silently dropped), exits cleanly, and the next
/// claim is free to proceed — one merge in flight.
#[test]
fn finish_hands_off_a_green_branch_stuck_in_the_queue() {
    let w = World::new("never");
    let out = w.finish_without_merging();
    assert!(
        out.status.success(),
        "a queued-but-green merge is a handoff, not a failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("still not on origin/main after"), "{err}");
    assert!(err.contains("receipt"), "{err}");
    // Closing is kept — the receipt keeps the debt — but the claim is freed so
    // the next task can be claimed while the merge queue lands this one.
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be released at handoff: {claims}"
    );
    let closed = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/closed/"],
    );
    assert!(
        closed.contains("refs/closed/T-WORKER-1"),
        "the close receipt must outlive the claim: {closed}"
    );
}

/// A lease that lapsed while the agent was away must not strand the work. The
/// old wait ran `tasks renew` under `set -e`, so the first renew after expiry
/// aborted finish: the PR was never published and the task became free for any
/// other agent to redo. finish now takes the same task back, keeping the scopes
/// it had reserved.
#[test]
fn a_lapsed_lease_is_re_adopted_with_its_scopes() {
    let w = World::new("lapsed");
    w.expire_claim();
    let out = w.finish_without_merging();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("taking it back"),
        "finish must re-adopt a lapsed claim: {err}"
    );
    assert!(
        err.contains("still not on origin/main after"),
        "it must get past the renew step: {err}"
    );
    // The re-adopted claim rode the wait out, and the queued-merge handoff
    // released it — the receipt is what survives.
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the handoff must release the claim it re-adopted: {claims}"
    );
}

/// The post-push wait's only value is catching a failed run while the claim
/// still makes repair cheap — once the run is green, the merge is pure
/// machinery (auto-merge, or the reconciler resyncing a green-but-behind
/// head) and parking the agent on it is dead time. `finish` releases the
/// claim as soon as the run reports green instead of waiting for the merge
/// commit; the close receipt keeps the debt until the sha lands on main.
#[test]
fn finish_hands_off_once_the_run_is_green() {
    let w = World::new("greenrun");
    let out = w.finish_with_a_green_run();
    assert!(
        out.status.success(),
        "a green run is a handoff, not a wait-out: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("green"), "{err}");
    assert!(
        !err.contains("still not on origin/main"),
        "the handoff happens on green, not at the budget: {err}"
    );
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be released at handoff: {claims}"
    );
    let closed = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/closed/"],
    );
    assert!(
        closed.contains("refs/closed/T-WORKER-1"),
        "the close receipt must outlive the claim: {closed}"
    );
}

/// Agents must never block on cloud CI: the branch-push run and the merge are
/// the machinery's, and nothing the agent does after the push speeds either
/// up. So `finish` pushes, publishes the close receipt, releases the claim and
/// returns — by default, with no `gh` and no poll — and the next task is free
/// to claim while the merge lands. A red run is reported at the next sync
/// (`scripts/check-owed-runs.sh`), where the agent passes before that claim.
#[test]
fn finish_hands_off_by_default_without_waiting() {
    let w = World::new("default-handoff");
    // A short budget, so a default that regressed to waiting fails here in
    // seconds (at the budget's handoff message) rather than after 30 minutes.
    let out = w.finish_env(
        false,
        &[
            ("TEST_NO_MERGE", "1"),
            ("SUSI_FINISH_WAIT_MAX", "3"),
            ("SUSI_FINISH_POLL", "1"),
        ],
    );
    assert!(
        out.status.success(),
        "an unmerged push is a handoff, not a failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    let both = format!("{err}\n{}", String::from_utf8_lossy(&out.stdout));
    assert!(err.contains("no wait"), "{err}");
    assert!(
        !both.contains("Waiting for") && !both.contains("still not on origin/main"),
        "the default must not enter the wait loop: {both}"
    );
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be released at handoff: {claims}"
    );
    let closed = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/closed/"],
    );
    assert!(
        closed.contains("refs/closed/T-WORKER-1"),
        "the close receipt must outlive the claim: {closed}"
    );

    // What the wait used to catch, the sync boundary catches: with `gh`
    // reporting the pushed head's run red, the owed-run check names it.
    // The run is keyed on the pushed tip (origin/worker), never on the head
    // the receipt recorded (the commit before the close): a `gh` that answers
    // only for the tip, and only when asked for exactly it.
    let tip = git(&w.remote, &["rev-parse", "refs/heads/worker"]);
    let receipt = git(&w.remote, &["cat-file", "-p", "refs/closed/T-WORKER-1"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_ne!(
        receipt["head"].as_str().unwrap(),
        tip,
        "the receipt's head is the accepted commit; the close commit is pushed on top"
    );
    let fake_gh = |conclusion: &str, url: &str| {
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = \"run list\" ] || exit 1\nwant=\nwhile [ $# -gt 0 ]; do [ \"$1\" = --commit ] && want=$2; shift; done\n[ \"$want\" = \"{tip}\" ] || {{ echo '[]'; exit 0; }}\nprintf '[{{\"headSha\":\"%s\",\"status\":\"completed\",\"conclusion\":\"{conclusion}\",\"url\":\"{url}\"}}]\\n' \"$want\"\n"
        )
    };
    executable(&w.bins.join("gh"), &fake_gh("failure", "https://ci/run/1"));
    let check = Command::new("bash")
        .arg(w.work.join("scripts/check-owed-runs.sh"))
        .current_dir(&w.work)
        .env("PATH", path_with_prefix(&w.bins))
        .env("SUSI_AGENT", "WORKER")
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "the owed-run check is advisory: {check:?}"
    );
    let report = String::from_utf8_lossy(&check.stderr);
    assert!(
        report.contains("did not pass")
            && report.contains("T-WORKER-1")
            && report.contains("https://ci/run/1"),
        "a red run for an owed receipt must be named at the sync boundary: {report}"
    );
    // A green run is silence.
    executable(&w.bins.join("gh"), &fake_gh("success", "https://ci/run/2"));
    let check = Command::new("bash")
        .arg(w.work.join("scripts/check-owed-runs.sh"))
        .current_dir(&w.work)
        .env("PATH", path_with_prefix(&w.bins))
        .env("SUSI_AGENT", "WORKER")
        .output()
        .unwrap();
    assert!(check.status.success());
    assert!(
        check.stderr.is_empty(),
        "a green run is nothing to report: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn finish_hands_off_immediately_with_nowait() {
    let w = World::new("nowait");
    let out = w.finish_env(
        false,
        &[("TEST_NO_MERGE", "1"), ("SUSI_FINISH_NOWAIT", "1")],
    );
    assert!(
        out.status.success(),
        "nowait hands off immediately: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(output.contains("no wait"), "{output}");
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be released at handoff: {claims}"
    );
    let closed = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/closed/"],
    );
    assert!(
        closed.contains("refs/closed/T-WORKER-1"),
        "the close receipt must outlive the claim: {closed}"
    );
}

/// A re-run of `finish` repays the whole gate even when nothing it judges
/// changed — a push rejected non-fast-forward by a stale remote head costs a
/// full scoped suite again. The stamp hashes the diff to origin/main with
/// `.agents/` bookkeeping excluded, so merging a remote head that only moves
/// task JSON leaves the verdict valid and the second finish skips the suite.
#[test]
fn a_gate_cache_hit_skips_the_scoped_suite() {
    let w = World::new("gate-cache");
    // A head already on our remote branch carrying only task bookkeeping, so
    // the first finish's push is rejected non-fast-forward.
    std::fs::write(
        w.primary.join(".agents/tasks/T-STALE-1.json"),
        "{\"id\":\"T-STALE-1\"}\n",
    )
    .unwrap();
    git(&w.primary, &["add", ".agents/tasks/T-STALE-1.json"]);
    git(&w.primary, &["commit", "-qm", "stale remote head"]);
    git(
        &w.primary,
        &["push", "-q", "origin", "HEAD:refs/heads/worker"],
    );
    git(&w.primary, &["reset", "--hard", "-q", "HEAD~1"]);

    let out = w.finish(false);
    assert!(
        !out.status.success(),
        "the non-fast-forward push must stop the first finish"
    );
    let gates_after_first = std::fs::read_to_string(w.root.join("gates")).unwrap();

    git(&w.work, &["fetch", "-q", "origin"]);
    git(
        &w.work,
        &["merge", "-q", "-m", "merge stale head", "origin/worker"],
    );
    let out = w.finish(false);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the retry must publish: {err}");
    assert!(
        err.contains("already passed"),
        "the second finish must not repay the suite: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(w.root.join("gates")).unwrap(),
        gates_after_first,
        "a stamp hit runs no gate commands at all"
    );
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        !claims.contains("refs/claims/T-WORKER-1"),
        "the merged close releases the claim: {claims}"
    );
}

/// A branch whose run has already failed will not merge, so waiting out the
/// budget is two hours of the agent's time spent not fixing it. `finish` stops
/// as soon as `gh` reports the failure for the sha it pushed — the same
/// fast-fail the closed-PR check gets, and the state that actually cost a round
/// of this session.
#[test]
fn finish_stops_when_its_own_run_has_failed() {
    let w = World::new("redrun");
    let out = w.finish_with_a_failing_run();
    assert!(!out.status.success(), "a red run must stop the wait");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("did not pass (completed/failure)"),
        "the loop must name the failed run, not the budget: {err}"
    );
    assert!(
        !err.contains("still not on origin/main"),
        "it must stop before the budget: {err}"
    );
    // Stopping early loses nothing: the closure and the claim stay.
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be retained: {claims}"
    );
}

/// The watcher is a background process that dies with its session, and the
/// primary checkout then stops converging until someone notices — the audit
/// found it five hours stale. `sync` is the boundary every agent crosses, so
/// the loop heals it there: this pins the decision and the restart itself,
/// with a stub in place of the watcher so no process outlives the test.
#[test]
fn the_sync_boundary_heals_a_dead_watcher() {
    let w = World::new("watcher");
    let stamp = w.primary.join(".git/susi-primary-watch.stamp");
    let script = w.primary.join("scripts/ensure-watcher.sh");

    // A stale heartbeat: the decision is to restart, and nothing else happens.
    std::fs::write(&stamp, "1\n").unwrap();
    let out = script_command(&script)
        .arg("--dry-run")
        .current_dir(&w.work)
        .output()
        .unwrap();
    assert!(!out.status.success(), "a stale watcher must be reported");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("would restart"), "{text}");

    // No heartbeat at all reads the same way.
    std::fs::remove_file(&stamp).unwrap();
    let out = script_command(&script)
        .arg("--dry-run")
        .current_dir(&w.work)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("never started"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The restart: the stub stands in for the watcher and writes the heartbeat
    // a real one would, so the heal is proven without leaving a process behind.
    let stub = w.root.join("bins/watcher-stub.sh");
    executable(
        &stub,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$(date +%s)\" > \"{}\"\n",
            stamp.display()
        ),
    );
    let out = script_command(&script)
        .current_dir(&w.work)
        .env("SUSI_WATCH_CMD", stub.to_str().unwrap())
        .env("SUSI_WATCH_STALE", "1")
        .env("SUSI_WATCH_WAIT", "5")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("restarted"), "the heal must say so: {text}");
    let fresh: u64 = std::fs::read_to_string(&stamp)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        now_unix().saturating_sub(fresh) <= 5,
        "the heartbeat must be fresh after the heal"
    );

    // A live watcher is left alone.
    let out = script_command(&script)
        .current_dir(&w.work)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("alive"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The board checks were only as reliable as the habit of running them, and two
/// of the three cost ~5s because they read the remote. So the verdict is cached
/// briefly: the first sync in a window pays, the rest read it — visible, and
/// never in the way.
#[test]
fn the_board_summary_is_cached_between_syncs() {
    let w = World::new("boardcache");
    let script = w.primary.join("scripts/swarm-status.sh");
    let run = || {
        let out = script_command(&script)
            .current_dir(&w.work)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let first = run();
    assert!(first.contains("swarm board:"), "{first}");
    let second = run();
    assert!(
        second.contains("board checked"),
        "the second sync must read the cache: {second}"
    );
    // A forced run checks again rather than reading the cache.
    let forced = script_command(&script)
        .arg("--force")
        .current_dir(&w.work)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&forced.stdout);
    assert!(text.contains("swarm board:"), "{text}");
    assert!(
        !text.contains("board checked"),
        "a forced run must not serve the cache: {text}"
    );
}
