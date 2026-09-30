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
        executable(&bins.join("git"), "#!/bin/sh\n/usr/bin/git \"$@\" || exit $?\nif [ \"$1\" = push ]; then\n for arg in \"$@\"; do\n  case \"$arg\" in HEAD:refs/heads/*) sha=$(/usr/bin/git rev-parse HEAD); /usr/bin/git --git-dir=\"$TEST_REMOTE\" update-ref refs/heads/main \"$sha\" ;; esac\n done\nfi\n");
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
