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
        for name in ["parallel-workflow.sh", "park-primary.sh"] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(name),
                scripts.join(name),
            )
            .unwrap();
        }
        executable(&scripts.join("accept.sh"), "#!/bin/sh\nexit 0\n");
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
        executable(&bins.join("cargo"), "#!/bin/sh\necho \"$1\" >> \"$TEST_GATES\"\nif [ \"$1\" = clippy ] && [ \"$TEST_GATE_FAIL\" = 1 ]; then exit 1; fi\n");
        // GitHub's successful PR merge is emulated only for the code branch push.
        executable(&bins.join("git"), "#!/bin/sh\n/usr/bin/git \"$@\" || exit $?\nif [ \"$1\" = push ] && [ \"$TEST_NO_MERGE\" != 1 ]; then\n for arg in \"$@\"; do\n  case \"$arg\" in HEAD:refs/heads/*) sha=$(/usr/bin/git rev-parse HEAD); /usr/bin/git --git-dir=\"$TEST_REMOTE\" update-ref refs/heads/main \"$sha\" ;; esac\n done\nfi\n");
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
            .env("TEST_NO_MERGE", "1")
            .env("SUSI_FINISH_POLL", "1")
            .env("SUSI_FINISH_WAIT_MAX", "3")
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
    assert_eq!(
        std::fs::read_to_string(w.root.join("gates")).unwrap(),
        "fmt\nclippy\ntest\n"
    );
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

/// The wait for publication is bounded. A branch that cannot merge — a red
/// gate, or a pull request the reconciler closes after 7 idle days — used to
/// hold the task claim forever, renewing its lease every 15 minutes, without
/// ever telling the agent. Now finish stops with a budget, keeps the closure
/// and the claim (so nobody else starts the work), and says what to do next.
#[test]
fn finish_gives_up_on_a_branch_that_never_merges() {
    let w = World::new("never");
    let out = w.finish_without_merging();
    assert!(
        !out.status.success(),
        "a bounded wait must fail rather than hang forever"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("still not on origin/main after"), "{err}");
    // Closing is kept, and so is the claim.
    assert!(w.work.join(".agents/tasks/done/T-WORKER-1.json").exists());
    let claims = git(
        &w.remote,
        &["for-each-ref", "--format=%(refname)", "refs/claims/"],
    );
    assert!(
        claims.contains("refs/claims/T-WORKER-1"),
        "the claim must be retained, not released: {claims}"
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
    let claim = git(&w.remote, &["cat-file", "-p", "refs/claims/T-WORKER-1"]);
    assert!(
        claim.contains("code.txt"),
        "the re-adopted claim dropped its scopes: {claim}"
    );
}
