#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test fixtures/assertions
#![allow(missing_docs)] // integration test crate
//! Exercise finish against a local bare remote, simulating its merge automation.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        for dir in [&primary, &remote, &bins, &root.join("home")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.name", "test"]);
        git(&primary, &["config", "user.email", "test@example.test"]);
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
            "ci-changed-crates.sh",
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
            "[package]\nname = \"fixture-root\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
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
        executable(&bins.join("cargo"), "#!/bin/sh\nif [ \"$1\" = metadata ]; then exec \"$TEST_REAL_CARGO\" \"$@\"; fi\necho \"$*\" >> \"$TEST_GATES\"\nif [ \"$1\" = clippy ] && [ \"$TEST_GATE_FAIL\" = 1 ]; then exit 1; fi\n");
        // GitHub's successful PR merge is emulated only for the code branch push.
        // With TEST_RACE_PRIMARY set, the first push instead lands the primary's
        // unpushed main commit on remote main — a merge that raced ours — so the
        // wait loop must integrate and repush (once) before ours can land.
        executable(&bins.join("git"), "#!/bin/sh\n/usr/bin/git \"$@\" || exit $?\nif [ \"$1\" = push ]; then\n for arg in \"$@\"; do\n  case \"$arg\" in HEAD:refs/heads/*)\n   if [ -n \"$TEST_RACE_PRIMARY\" ] && [ ! -f \"$TEST_RACE_USED\" ]; then\n    : > \"$TEST_RACE_USED\"\n    (cd \"$TEST_RACE_PRIMARY\" && /usr/bin/git push -q origin main)\n   elif [ \"$TEST_NO_MERGE\" != 1 ]; then\n    sha=$(/usr/bin/git rev-parse HEAD); /usr/bin/git --git-dir=\"$TEST_REMOTE\" update-ref refs/heads/main \"$sha\"\n   fi ;;\n  esac\n done\nfi\n");
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
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/xdg"))
            .env_remove("SUSI_HOME")
            .env("SUSI_AGENT", "WORKER");
        cmd
    }
    fn finish(&self, fail: bool) -> std::process::Output {
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env(
                "PATH",
                format!("{}:{}", self.bins.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", if fail { "1" } else { "0" })
            // A gate failure needs a gate that runs: the fixture's work
            // touches no crate, so the scoped gate skips clippy entirely.
            .env("SUSI_LOCAL_GATE", if fail { "full" } else { "scoped" })
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .output()
            .unwrap()
    }

    /// `finish` while another merge lands on main exactly as our branch
    /// pushes: the wait loop must integrate the race and repush — without
    /// re-running the gate, which is what made every merge a multiplier.
    fn finish_racing_an_intervening_merge(&self) -> std::process::Output {
        // The competitor's commit lives on the primary's main but is unpushed;
        // the fake remote lands it when our branch push arrives.
        git(
            &self.primary,
            &["commit", "--allow-empty", "-qm", "someone else merged"],
        );
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env(
                "PATH",
                format!("{}:{}", self.bins.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_RACE_PRIMARY", &self.primary)
            .env("TEST_RACE_USED", self.root.join("race-used"))
            .env("SUSI_FINISH_WAIT_MAX", "60")
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

    /// `finish` on a branch whose push never becomes a merge, with a one-second
    /// poll and a three-second budget so the wait is observable in a test.
    fn finish_without_merging(&self) -> std::process::Output {
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env(
                "PATH",
                format!("{}:{}", self.bins.display(), std::env::var("PATH").unwrap()),
            )
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

    /// `finish` where the branch never merges *and* `gh` reports that the run
    /// for the pushed sha has failed. The budget is larger than the run-check
    /// cadence, so a passing test proves the loop stopped on the failure rather
    /// than on the budget.
    fn finish_with_a_failing_run(&self) -> std::process::Output {
        executable(
            &self.bins.join("gh"),
            "#!/bin/sh\ncase \"$1 $2\" in\n\"run list\")\n sha=$(/usr/bin/git rev-parse HEAD 2>/dev/null)\n printf '[{\"headSha\":\"%s\",\"status\":\"completed\",\"conclusion\":\"failure\"}]\\n' \"$sha\"\n ;;\n\"pr view\") echo OPEN ;;\n*) exit 1 ;;\nesac\n",
        );
        self.susi(&["workflow", "finish", "T-WORKER-1"])
            .env(
                "PATH",
                format!("{}:{}", self.bins.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_REMOTE", &self.remote)
            .env("TEST_GATES", self.root.join("gates"))
            .env("TEST_GATE_FAIL", "0")
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env("TEST_NO_MERGE", "1")
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
    let out = w.finish(false);
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

/// A diff that touches a member crate gates that crate — clippy and test
/// both carry `-p cell` and nothing runs `--workspace`. Before the gate was
/// scoped, this task paid for the whole workspace like every other.
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
        gates.contains("test --locked -p cell"),
        "scoped tests: {gates}"
    );
    assert!(!gates.contains("--workspace"), "{gates}");
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

/// When another merge lands between our gate and our push, finish integrates
/// it and repushes — once, without re-running the gate. The remote merge gate
/// only accepts heads that contain main and whose branch run is green, so the
/// merged head is proven by its own CI run; the old re-gate multiplied the
/// whole workspace suite by every merge that raced the wait.
#[test]
fn an_intervening_merge_does_not_re_run_the_gate() {
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
    // And the branch did merge: the raced head was integrated and repushed.
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
    let out = Command::new(&script)
        .arg("--dry-run")
        .current_dir(&w.work)
        .output()
        .unwrap();
    assert!(!out.status.success(), "a stale watcher must be reported");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("would restart"), "{text}");

    // No heartbeat at all reads the same way.
    std::fs::remove_file(&stamp).unwrap();
    let out = Command::new(&script)
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
    let out = Command::new(&script)
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
    let out = Command::new(&script).current_dir(&w.work).output().unwrap();
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
        let out = Command::new(&script).current_dir(&w.work).output().unwrap();
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
    let forced = Command::new(&script)
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
