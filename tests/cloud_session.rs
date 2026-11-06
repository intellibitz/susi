//! A cloud session (Claude Code on the web) works in the clone its harness
//! made: a primary checkout on a feature branch, in a disposable container,
//! with no laptop behind it. The loop used to refuse exactly that shape — "do
//! not commit in the primary checkout", "scripts/susi-worktree.sh", a watcher
//! for a primary nobody parks — and every fix it printed pointed at a machine
//! the session cannot reach. These run the real binary, hooks and scripts
//! against that shape, marked the way the session-start hook marks it.
//!
//! The mark lives on the clone (`git config susi.cloud`), never in the
//! environment: these tests run inside cloud containers too, and a fixture
//! must behave like a laptop's clone until it is marked.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document
#![cfg(unix)] // after the allows: elsewhere it empties the crate, allows and docs too

use std::path::{Path, PathBuf};
use std::process::Command;
use susi_paths::test_env::isolate_home;

const HOOKS: [&str; 5] = [
    "commit-msg",
    "pre-commit",
    "pre-push",
    "workflow-guard",
    "run-susi",
];
const SCRIPTS: [&str; 14] = [
    "cloud-session.sh",
    "parallel-workflow.sh",
    "cloud-onnxruntime.sh",
    "cloud-target-seed.sh",
    "check-worker-identity.sh",
    "check-task-scope.sh",
    "park-primary.sh",
    "ensure-watcher.sh",
    "susi-worktree.sh",
    "setup-dev.sh",
    "posix-compat.sh",
    "workflow-session-start.sh",
    "link-shared-target.sh",
    "target-guard.sh",
];

/// The variables a worker's shell may carry and these fixtures must not: the
/// cloud harness's own marker, the explicit word, a borrowed identity, the
/// hooks' escape hatches, the queue transport's knobs, and the `GIT_DIR` a
/// calling hook exports.
const SCRUBBED: [&str; 18] = [
    "SUSI_QUEUE_TRANSPORT",
    "SUSI_QUEUE_WAIT_SECS",
    "SUSI_QUEUE_POLL_MS",
    "SUSI_HOME",
    "SUSI_AGENT",
    "SUSI_CLOUD_SESSION",
    "SUSI_CLOUD_TOOL",
    "SUSI_ALLOW_PRIMARY",
    "SUSI_ALLOW_MAIN",
    "SUSI_TASK_REMOTE",
    "CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE",
    "CLAUDE_PROJECT_DIR",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
];

struct World {
    root: PathBuf,
    clone: PathBuf,
    home: PathBuf,
}

fn finish(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

impl World {
    /// A bare server and the clone a cloud harness makes of it: the primary
    /// checkout, on the harness's branch, with this repository's real hooks and
    /// scripts committed on `main`, and the shared git login as `user.name`.
    fn new(tag: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root =
            std::env::temp_dir().join(format!("susi-cloud-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let clone = root.join("susi");
        let home = root.join("home");
        for dir in [&bare, &clone, &home] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let w = Self { root, clone, home };
        w.git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        w.git(&w.clone, &["init", "--quiet", "-b", "main"]);
        w.git(&w.clone, &["config", "user.name", "INTELLIBITZ"]);
        w.git(&w.clone, &["config", "user.email", "login@localhost"]);
        w.git(&w.clone, &["config", "commit.gpgsign", "false"]);
        w.git(
            &w.clone,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(w.clone.join(".githooks")).unwrap();
        for hook in HOOKS {
            std::fs::copy(
                repo.join(".githooks").join(hook),
                w.clone.join(".githooks").join(hook),
            )
            .unwrap();
        }
        std::fs::create_dir_all(w.clone.join("scripts")).unwrap();
        for script in SCRIPTS {
            std::fs::copy(
                repo.join("scripts").join(script),
                w.clone.join("scripts").join(script),
            )
            .unwrap();
        }
        std::fs::create_dir_all(w.clone.join("work")).unwrap();
        std::fs::write(w.clone.join("work/README"), "fixture\n").unwrap();
        w.git(&w.clone, &["add", "-A"]);
        w.git(&w.clone, &["commit", "--quiet", "-m", "init"]);
        w.git(&w.clone, &["push", "--quiet", "origin", "HEAD:main"]);
        w.git(&w.clone, &["fetch", "--quiet", "origin"]);
        // What the harness does: check the work branch out in the clone itself.
        w.git(&w.clone, &["switch", "--quiet", "-c", "ccr-test"]);
        w.git(&w.clone, &["config", "core.hooksPath", ".githooks"]);
        w
    }

    /// A worker's command: a private HOME, no system git config, none of the
    /// variables the cloud harness or a test runner would hand down.
    fn command(&self, program: &str, dir: &Path) -> Command {
        let mut cmd = Command::new(program);
        isolate_home(&mut cmd, &self.home)
            .current_dir(dir)
            // The hooks exec `susi workflow guard` (.githooks/run-susi): the
            // binary under test, not whatever susi the host has installed.
            .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("SUSI_PRIMARY_WATCH_DISABLE", "1");
        for var in SCRUBBED {
            cmd.env_remove(var);
        }
        cmd
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = finish(self.command("git", dir).args(args));
        assert_eq!(code, 0, "git {args:?}: {err}");
        out
    }

    fn susi(&self, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_susi"), &self.clone);
        cmd.args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        finish(&mut cmd)
    }

    fn script(&self, name: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
        let path = self.clone.join(name);
        let mut cmd = self.command(path.to_str().unwrap(), &self.clone);
        cmd.args(args);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        finish(&mut cmd)
    }

    fn config(&self, key: &str) -> Option<String> {
        let (code, out, _) = finish(
            self.command("git", &self.clone)
                .args(["config", "--get", key]),
        );
        (code == 0).then_some(out)
    }

    fn mark(&self) {
        let (code, out, err) = self.script(
            "scripts/cloud-session.sh",
            &["--mark"],
            &[("CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE", "cloud_default")],
        );
        assert_eq!(code, 0, "{err}");
        assert!(
            out.contains("this clone is the worker's own worktree"),
            "{out}"
        );
    }

    fn bare(&self) -> PathBuf {
        self.root.join("server.git")
    }

    /// The cloud proxy, as the server sees it: a push outside `refs/heads/*`
    /// is refused, and so is any branch deletion.
    fn refuse_queue_pushes(&self) {
        use std::os::unix::fs::PermissionsExt;
        let hooks = self.bare().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\n\
             while read -r _old new ref; do\n\
             \tcase \"$new\" in *[!0]*) ;; *)\n\
             \t\techo \"HTTP 403: this transport deletes nothing ($ref)\" >&2; exit 1 ;;\n\
             \tesac\n\
             \tcase \"$ref\" in refs/heads/*) ;; *)\n\
             \t\techo \"HTTP 403: this transport carries refs/heads/* only ($ref)\" >&2; exit 1 ;;\n\
             \tesac\n\
             done\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cloud_session_mark_is_recorded_on_the_clone_not_read_from_the_environment() {
    let w = World::new("mark");
    // A fresh clone is a laptop's clone, whatever the environment says.
    let (code, _, _) = w.script("scripts/cloud-session.sh", &[], &[]);
    assert_eq!(code, 1);
    let (code, _, _) = w.script(
        "scripts/cloud-session.sh",
        &[],
        &[("CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE", "cloud_default")],
    );
    assert_eq!(
        code, 1,
        "the harness's variable alone must not make a clone cloud"
    );
    // Marking needs the harness's word, and records the tool with it.
    let (code, _, _) = w.script("scripts/cloud-session.sh", &["--mark"], &[]);
    assert_eq!(code, 1);
    assert_eq!(w.config("susi.cloud"), None);
    let (code, out, _) = w.script(
        "scripts/cloud-session.sh",
        &["--detect"],
        &[("CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE", "cloud_default")],
    );
    assert_eq!(code, 0);
    assert_eq!(out, "claude");
    assert_eq!(w.config("susi.cloud"), None, "--detect marks nothing");
    w.mark();
    assert_eq!(w.config("susi.cloud").as_deref(), Some("true"));
    assert_eq!(w.config("susi.cloudTool").as_deref(), Some("CLAUDE"));
    let (code, _, _) = w.script("scripts/cloud-session.sh", &[], &[]);
    assert_eq!(code, 0);
    let (code, out, _) = w.script("scripts/cloud-session.sh", &["--tool"], &[]);
    assert_eq!(code, 0);
    assert_eq!(out, "CLAUDE");
    // The explicit word beats the mark, both ways.
    let (code, _, _) = w.script(
        "scripts/cloud-session.sh",
        &[],
        &[("SUSI_CLOUD_SESSION", "0")],
    );
    assert_eq!(code, 1);
    let other = World::new("mark-explicit");
    let (code, _, _) = other.script(
        "scripts/cloud-session.sh",
        &[],
        &[("SUSI_CLOUD_SESSION", "1")],
    );
    assert_eq!(code, 0);
    // `--mark --force` records an explicit session that no harness variable names.
    let (code, _, _) = other.script(
        "scripts/cloud-session.sh",
        &["--mark", "--force"],
        &[("SUSI_CLOUD_TOOL", "codex")],
    );
    assert_eq!(code, 0);
    assert_eq!(other.config("susi.cloudTool").as_deref(), Some("CODEX"));
}

#[test]
fn cloud_session_the_binary_and_the_script_read_the_same_answer() {
    let w = World::new("agree");
    // Unmarked: the primary checkout is refused, with the cloud fix named.
    let (code, out, _) = w.susi(&["workflow", "check"], &[]);
    assert_ne!(code, 0);
    assert!(out.contains("❌ own worktree"), "{out}");
    assert!(out.contains("scripts/cloud-session.sh --mark"), "{out}");
    assert!(out.contains("⚠️  primary watcher"), "{out}");
    // The explicit word is honoured by both without a mark…
    let (_, out, _) = w.susi(&["workflow", "check"], &[("SUSI_CLOUD_SESSION", "1")]);
    assert!(out.contains("✅ own worktree"), "{out}");
    let (code, _, _) = w.script(
        "scripts/cloud-session.sh",
        &[],
        &[("SUSI_CLOUD_SESSION", "1")],
    );
    assert_eq!(code, 0);
    // …and overrides the mark in both.
    w.mark();
    let (_, out, _) = w.susi(&["workflow", "check"], &[("SUSI_CLOUD_SESSION", "0")]);
    assert!(out.contains("❌ own worktree"), "{out}");
    let (_, out, _) = w.susi(&["workflow", "check"], &[]);
    assert!(out.contains("✅ own worktree"), "{out}");
}

#[test]
fn cloud_session_workflow_check_accepts_the_clone_as_the_workers_own() {
    let w = World::new("check");
    // What a cloud harness's global git config carries: its signing identity.
    std::fs::write(
        w.home.join(".gitconfig"),
        "[user]\n\tname = Claude\n\temail = harness@example.invalid\n",
    )
    .unwrap();
    w.mark();
    let (code, out, err) = w.susi(&["workflow", "check"], &[]);
    assert!(
        out.contains("✅ own worktree") && out.contains("cloud session"),
        "out={out} err={err}"
    );
    // The identity is the harness's tool plus the branch, never the login.
    assert!(out.contains("(agent CLAUDECCRTEST)"), "{out}");
    assert!(
        out.contains("✅ own identity") && out.contains("assigned CLAUDECCRTEST"),
        "{out}"
    );
    assert_eq!(
        w.git(&w.clone, &["config", "--worktree", "--get", "user.name"]),
        "CLAUDECCRTEST"
    );
    assert_eq!(
        w.git(&w.clone, &["config", "--worktree", "--get", "user.email"]),
        "harness@example.invalid"
    );
    // Nothing to keep converging, so no watcher is asked for; the only ❌ left
    // is the claim every worker needs.
    assert!(out.contains("✅ primary watcher"), "{out}");
    assert!(out.contains("❌ holds a claim"), "{out}");
    assert_ne!(code, 0);

    // The whole loop from here is the ordinary one: mint a task under the
    // clone's own identity, commit it (the hooks run: this is the primary
    // checkout, and they allow it), claim it, and the check is green.
    let (code, out, err) = w.susi(
        &["tasks", "add", "cloud work", "--accept", "cargo --version"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("added T-CLAUDECCRTEST-1"), "{out}");
    w.git(&w.clone, &["add", ".agents/tasks/T-CLAUDECCRTEST-1.json"]);
    w.git(&w.clone, &["commit", "--quiet", "-m", "task"]);
    let (code, _, err) = w.susi(
        &["tasks", "claim", "T-CLAUDECCRTEST-1", "--scope", "work"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = w.susi(&["workflow", "check"], &[]);
    assert_eq!(code, 0, "out={out} err={err}");
    assert!(
        out.contains("✅ holds a claim") && out.contains("ready:"),
        "{out}"
    );

    // A commit of real work passes every hook: the worktree guard (primary,
    // but a cloud session's), the trailer, the identity and the scope checks.
    std::fs::write(w.clone.join("work/change.txt"), "done\n").unwrap();
    w.git(&w.clone, &["add", "work/change.txt"]);
    let (code, _, err) = finish(w.command("git", &w.clone).args([
        "commit",
        "--quiet",
        "-m",
        "work: a change",
        "-m",
        "Task: T-CLAUDECCRTEST-1",
    ]));
    assert_eq!(code, 0, "{err}");
    // The author is the worker; the email is the harness's signing identity
    // (the fixture's global config), so GitHub shows the commit as verified.
    assert_eq!(
        w.git(&w.clone, &["log", "-1", "--format=%an <%ae>"]),
        "CLAUDECCRTEST <harness@example.invalid>"
    );
    // `workflow start` has nothing to create: it hands the clone back.
    let (code, out, err) = w.susi(&["workflow", "start"], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out.lines().last().unwrap(),
        format!("cd {}", w.clone.display())
    );
    assert_eq!(
        w.git(&w.clone, &["worktree", "list"]).lines().count(),
        1,
        "no worktree is created for a cloud session"
    );
}

#[test]
fn cloud_session_hooks_allow_commits_in_the_marked_clone_but_never_on_main() {
    let w = World::new("hooks");
    let guard = |envs: &[(&str, &str)]| w.script(".githooks/workflow-guard", &["commit"], envs);
    let (code, _, err) = guard(&[]);
    assert_ne!(code, 0, "an unmarked primary checkout is refused");
    assert!(
        err.contains("do not commit in the primary checkout"),
        "{err}"
    );
    assert!(err.contains("scripts/cloud-session.sh --mark"), "{err}");
    assert!(!err.contains("syntax error"), "{err}");
    w.mark();
    let (code, _, err) = guard(&[]);
    assert_eq!(code, 0, "{err}");
    // The explicit word turns it back into a laptop's clone.
    let (code, _, _) = guard(&[("SUSI_CLOUD_SESSION", "0")]);
    assert_ne!(code, 0);
    // main is nobody's place, cloud or not.
    w.git(&w.clone, &["switch", "--quiet", "main"]);
    let (code, _, err) = guard(&[]);
    assert_ne!(code, 0);
    assert!(err.contains("no direct commits on main"), "{err}");
    w.git(&w.clone, &["switch", "--quiet", "ccr-test"]);
}

#[test]
fn cloud_session_identity_hook_judges_the_primary_checkout_as_a_workers_place() {
    let w = World::new("identity");
    w.mark();
    // A borrowed role as token: on a laptop the primary checkout is skipped
    // (nobody commits there); a cloud session's clone is a worker's place.
    w.git(&w.clone, &["config", "extensions.worktreeConfig", "true"]);
    w.git(&w.clone, &["config", "--worktree", "susi.agent", "PRIMARY"]);
    w.git(&w.clone, &["config", "--worktree", "user.name", "PRIMARY"]);
    let (code, _, err) = w.script("scripts/check-worker-identity.sh", &[], &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("shared git login or a role"), "{err}");
    // The documented fix gives it its own.
    let (code, out, err) = w.susi(&["workflow", "own-identity"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("CLAUDECCRTEST"), "{out}");
    let (code, out, err) = w.script("scripts/check-worker-identity.sh", &[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("CLAUDECCRTEST is this worktree's token alone"),
        "{out}"
    );
    // No harness email in this fixture's global config: the worker's own.
    assert_eq!(
        w.git(&w.clone, &["config", "--worktree", "--get", "user.email"]),
        "claudeccrtest@localhost"
    );
}

#[test]
fn cloud_session_scripts_leave_the_clone_where_it_is() {
    let w = World::new("scripts");
    w.mark();
    // Parking would switch the work branch away: refused, politely.
    let (code, out, err) = w.script("scripts/park-primary.sh", &[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("nothing to park"), "{out}");
    assert_eq!(
        w.git(&w.clone, &["symbolic-ref", "--short", "HEAD"]),
        "ccr-test"
    );
    let (code, out, _) = w.script("scripts/park-primary.sh", &["--sync-only"], &[]);
    assert_eq!(code, 0);
    assert!(out.is_empty(), "--sync-only is silent: {out}");
    // No primary to converge, no watcher to heal.
    let (code, out, err) = w.script("scripts/ensure-watcher.sh", &[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("not needed"), "{out}");
    // The worktree script hands the clone back instead of making a sibling.
    let (code, out, err) = w.script("scripts/susi-worktree.sh", &[], &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out.lines().last().unwrap(),
        format!("cd {}", w.clone.display())
    );
    assert!(err.contains("this clone is your worktree"), "{err}");
    assert_eq!(w.git(&w.clone, &["worktree", "list"]).lines().count(), 1);
    assert_eq!(
        w.git(&w.clone, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    // On main there is nothing to hand back: the fix is a branch of your own.
    w.git(&w.clone, &["switch", "--quiet", "main"]);
    let (code, _, err) = w.script("scripts/susi-worktree.sh", &[], &[]);
    assert_ne!(code, 0);
    assert!(err.contains("git switch -c <your-branch>"), "{err}");
    w.git(&w.clone, &["switch", "--quiet", "ccr-test"]);
}

#[test]
fn cloud_session_start_hook_marks_only_the_project_it_was_started_for() {
    let w = World::new("hook");
    let quiet = [
        ("SUSI_CLOUD_PREBUILD", "0"),
        ("SUSI_CLOUD_ONNXRUNTIME", "0"),
        ("SUSI_CLOUD_SEED", "0"),
    ];
    // The harness's variable without the hook's own project: a fixture a test
    // runs the script in, or a stray shell. Nothing is marked, and the laptop
    // guidance is printed as before.
    let mut envs = quiet.to_vec();
    envs.push(("CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE", "cloud_default"));
    let (code, out, err) = w.script("scripts/workflow-session-start.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert_eq!(w.config("susi.cloud"), None, "{out}");
    assert!(out.contains("scripts/susi-worktree.sh"), "{out}");
    assert!(!out.contains("Cloud session:"), "{out}");
    // The project the hook was started for, in a cloud environment: marked
    // once, and the session is told what a cloud session does instead.
    let project = w.clone.to_str().unwrap().to_string();
    envs.push(("CLAUDE_PROJECT_DIR", project.as_str()));
    let (code, out, err) = w.script("scripts/workflow-session-start.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert_eq!(w.config("susi.cloud").as_deref(), Some("true"), "{out}");
    assert!(
        out.contains("Cloud session: this clone is your worktree"),
        "{out}"
    );
    assert!(out.contains("never ask for the operator's laptop"), "{out}");
    assert!(!out.contains("Not ready? In the primary checkout"), "{out}");
    // Marked, the hook needs no variable at all to know where it is.
    let (code, out, _) = w.script("scripts/workflow-session-start.sh", &[], &quiet);
    assert_eq!(code, 0);
    assert!(out.contains("Cloud session:"), "{out}");
}

/// The snapshot key names what the kept units' fingerprints depend on: the
/// external packages of the lockfile, the toolchain pin, the compiler and the
/// cloud's cargo configuration — and not the workspace's own packages, which the
/// snapshot never carries. The workflow and the hook compute it with this one
/// script, so a snapshot is found by the checkout it was built for.
#[cfg(target_os = "linux")]
#[test]
fn cloud_target_seed_key_follows_the_external_lockfile_and_the_toolchain() {
    let w = World::new("seedkey");
    let lock = "version = 4\n\n[[package]]\nname = \"susi\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aa\"\n";
    std::fs::write(w.clone.join("Cargo.lock"), lock).unwrap();
    std::fs::write(
        w.clone.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.0\"\n",
    )
    .unwrap();
    let key = |w: &World| {
        let (code, out, err) = w.script("scripts/cloud-target-seed.sh", &["--key"], &[]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out.len(), 16, "a 16-hex key: {out}");
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()), "{out}");
        out
    };
    let base = key(&w);
    assert_eq!(key(&w), base, "stable across runs");
    let (code, name, _) = w.script("scripts/cloud-target-seed.sh", &["--name"], &[]);
    assert_eq!(code, 0);
    assert_eq!(name, format!("linux-{}-{base}", std::env::consts::ARCH));
    // The workspace's own package changes nothing: it is never in the snapshot.
    std::fs::write(w.clone.join("Cargo.lock"), lock.replace("0.1.0", "0.2.0")).unwrap();
    assert_eq!(key(&w), base, "a path package's version is not a key input");
    // An external package does.
    std::fs::write(w.clone.join("Cargo.lock"), lock.replace("1.0.0", "1.0.1")).unwrap();
    let bumped = key(&w);
    assert_ne!(bumped, base, "a registry package's version is a key input");
    std::fs::write(w.clone.join("Cargo.lock"), lock).unwrap();
    assert_eq!(key(&w), base);
    // So does the toolchain pin.
    std::fs::write(
        w.clone.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.100.0\"\n",
    )
    .unwrap();
    assert_ne!(key(&w), base, "the toolchain pin is a key input");
}

/// The assets live on the origin's own `target-seed` release, whatever spelling
/// the clone URL has: with or without `.git`, over https or ssh. The first
/// measured seeding looked under `susi.git/releases/…`, which does not exist,
/// and fell back to a cold build.
#[cfg(target_os = "linux")]
#[test]
fn cloud_target_seed_url_is_the_origins_release_without_a_git_suffix() {
    let w = World::new("seedurl");
    for (origin, want) in [
        (
            "https://github.com/acme/thing.git",
            "https://github.com/acme/thing/releases/download/target-seed",
        ),
        (
            "https://github.com/acme/thing",
            "https://github.com/acme/thing/releases/download/target-seed",
        ),
        (
            "git@github.com:acme/thing.git",
            "https://github.com/acme/thing/releases/download/target-seed",
        ),
    ] {
        w.git(&w.clone, &["remote", "set-url", "origin", origin]);
        let (code, out, err) = w.script("scripts/cloud-target-seed.sh", &["--url"], &[]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out, want, "origin {origin}");
    }
    let (code, out, _) = w.script(
        "scripts/cloud-target-seed.sh",
        &["--url"],
        &[("SUSI_TARGET_SEED_URL", "file:///elsewhere")],
    );
    assert_eq!(code, 0);
    assert_eq!(out, "file:///elsewhere", "an explicit source wins");
}

/// The hook's seeding: a published snapshot (parts of one gzip'd tar with a
/// sha256sum manifest) is fetched, verified and unpacked into an empty target/,
/// marked as seeded; a target that holds anything is left alone; a part that
/// fails its checksum seeds nothing; and SUSI_CLOUD_SEED=0 skips it all. A
/// `file://` URL stands in for the release here — curl reads it the same way.
#[cfg(target_os = "linux")]
#[test]
fn cloud_target_seed_unpacks_a_verified_snapshot_into_an_empty_target_only() {
    let w = World::new("seedtar");
    std::fs::write(w.clone.join("Cargo.lock"), "version = 4\n").unwrap();
    let (code, name, err) = w.script("scripts/cloud-target-seed.sh", &["--name"], &[]);
    assert_eq!(code, 0, "{err}");
    // A snapshot the way `seed-target --export` lays one out.
    let snap = w.root.join("snap");
    std::fs::create_dir_all(snap.join("debug/.fingerprint/serde-0123456789abcdef")).unwrap();
    std::fs::create_dir_all(snap.join("debug/deps")).unwrap();
    std::fs::write(snap.join("debug/deps/libserde-0123456789abcdef.rlib"), b"").unwrap();
    std::fs::write(
        snap.join(".susi-seed"),
        "0123456789abcdef0123456789abcdef01234567 1700000000\n",
    )
    .unwrap();
    let assets = w.root.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let sh = |script: &str| {
        let out = Command::new("bash")
            .args(["-euo", "pipefail", "-c", script])
            .current_dir(&assets)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    // Two parts, so the concatenation is exercised; the manifest under this
    // key's name and under the `latest` name both.
    sh(&format!(
        "head -c 70000 /dev/urandom > '{0}/debug/deps/libserde-0123456789abcdef.rlib' && tar -C '{0}' -cf - . | gzip -1 | split -b 40000 -d -a 2 - '{name}.tar.gz.part' && sha256sum {name}.tar.gz.part* > '{name}.manifest' && cp '{name}.manifest' linux-{arch}-latest.manifest && test -f '{name}.tar.gz.part01'",
        snap.display(),
        arch = std::env::consts::ARCH,
    ));
    let url = format!("file://{}", assets.display());
    let envs = [("SUSI_TARGET_SEED_URL", url.as_str())];

    // Skipped on request: nothing fetched, no target made.
    let (code, _, err) = w.script(
        "scripts/cloud-target-seed.sh",
        &[],
        &[
            ("SUSI_TARGET_SEED_URL", url.as_str()),
            ("SUSI_CLOUD_SEED", "0"),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("skipped"), "{err}");
    assert!(!w.clone.join("target").exists());

    // An explicitly selected output still prevents seeding the wrong target.
    let external_target = w.root.join("external-target");
    let (code, _, err) = w.script(
        "scripts/cloud-target-seed.sh",
        &[],
        &[
            ("SUSI_TARGET_SEED_URL", url.as_str()),
            ("CARGO_TARGET_DIR", external_target.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("CARGO_TARGET_DIR"), "{err}");
    assert!(!w.clone.join("target").exists());
    assert!(!external_target.exists());

    // Seeded: the snapshot's units are in target/, and the markers say so.
    let (code, _, err) = w.script("scripts/cloud-target-seed.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("seeded target"), "{err}");
    assert!(err.contains("snapshot for this key"), "{err}");
    assert_eq!(
        std::fs::read(
            w.clone
                .join("target/debug/deps/libserde-0123456789abcdef.rlib")
        )
        .unwrap()
        .len(),
        70_000
    );
    assert!(w
        .clone
        .join("target/debug/.fingerprint/serde-0123456789abcdef")
        .is_dir());
    assert!(w.clone.join("target/.susi-seed").is_file());
    assert!(
        std::fs::read_to_string(w.clone.join("target/.susi-seeded"))
            .unwrap()
            .contains("seeded from"),
        "the seeded marker names the source"
    );
    assert!(
        std::fs::read_dir(&w.clone).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("target.seed.")),
        "the download directory is removed"
    );

    // A target that holds a build is never replaced.
    std::fs::remove_dir_all(w.clone.join("target/debug")).unwrap();
    std::fs::write(w.clone.join("target/mine"), b"a build in progress").unwrap();
    let (code, _, err) = w.script("scripts/cloud-target-seed.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("already holds a build"), "{err}");
    assert!(w.clone.join("target/mine").is_file());
    assert!(!w.clone.join("target/debug").exists());
    std::fs::remove_dir_all(w.clone.join("target")).unwrap();

    // No snapshot for this key: the newest one is taken, and said so.
    std::fs::remove_file(assets.join(format!("{name}.manifest"))).unwrap();
    let (code, _, err) = w.script("scripts/cloud-target-seed.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("taking the newest"), "{err}");
    assert!(w.clone.join("target/.susi-seed").is_file());
    std::fs::remove_dir_all(w.clone.join("target")).unwrap();

    // A part that fails its checksum seeds nothing.
    let latest = assets.join(format!("linux-{}-latest.manifest", std::env::consts::ARCH));
    let listed = std::fs::read_to_string(&latest).unwrap();
    std::fs::write(&latest, format!("deadbeef{}", &listed[8..])).unwrap();
    let (code, _, err) = w.script("scripts/cloud-target-seed.sh", &[], &envs);
    assert_eq!(code, 0, "the seeding never fails its caller: {err}");
    assert!(err.contains("does not match its checksum"), "{err}");
    assert!(
        !w.clone.join("target").exists()
            || std::fs::read_dir(w.clone.join("target"))
                .unwrap()
                .next()
                .is_none(),
        "nothing seeded"
    );

    // Nothing published at all: a cold build, said plainly.
    let (code, _, err) = w.script(
        "scripts/cloud-target-seed.sh",
        &[],
        &[("SUSI_TARGET_SEED_URL", "file:///nonexistent/susi-seed")],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no snapshot published"), "{err}");
}

/// The session-start hook seeds before it builds, inside the one background
/// subshell that holds the clone's build lock, and tells the session how to
/// wait for both.
#[test]
fn cloud_target_seed_runs_in_the_start_hook_before_the_background_build() {
    let hook = std::fs::read_to_string("scripts/workflow-session-start.sh").unwrap();
    let seed = hook
        .find(
            "scripts/cloud-target-seed.sh
",
        )
        .expect("the hook seeds");
    let build = hook.find("exec cargo build -q").expect("the hook builds");
    assert!(
        seed < build,
        "the seeding comes before the build in the same subshell"
    );
    assert!(
        hook.contains("SUSI_CLOUD_SEED"),
        "SUSI_CLOUD_SEED=0 opts out of the seeding"
    );
    assert!(
        hook.contains("wait for both with: scripts/cloud-target-seed.sh --wait"),
        "the session is told how to wait for the seeding and the build"
    );
}

#[cfg(target_os = "linux")]
#[test]
// Compatibility test for the retained legacy script, not a native development prerequisite.
fn legacy_cloud_onnxruntime_does_not_touch_the_repository() {
    let w = World::new("onnx");
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => panic!("no ONNX Runtime release for {other}"),
    };
    let version = "1.28.0";
    let base = w.home.join(".cache/susi/onnxruntime");
    let lib = base.join(format!("onnxruntime-linux-{arch}-{version}/lib"));
    // The library is already there: nothing is downloaded, and cargo is told.
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("libonnxruntime.so"), b"").unwrap();
    let cargo_home = w.home.join(".cargo");
    let envs = [("CARGO_HOME", cargo_home.to_str().unwrap())];
    let (code, _, err) = w.script("scripts/cloud-onnxruntime.sh", &["--check"], &envs);
    assert_ne!(code, 0, "nothing configured yet: {err}");
    let (code, out, err) = w.script("scripts/cloud-onnxruntime.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("already at"), "{out}");
    let config = std::fs::read_to_string(cargo_home.join("config.toml")).unwrap();
    assert!(
        config.contains(&format!("ORT_LIB_LOCATION = \"{}\"", lib.display())),
        "{config}"
    );
    assert!(
        config.contains("ORT_PREFER_DYNAMIC_LINK = \"1\""),
        "{config}"
    );
    assert!(
        config.contains(&format!("link-arg=-Wl,-rpath,{}", lib.display())),
        "{config}"
    );
    // Idempotent: a second run replaces its own block, nothing accumulates.
    let (code, _, err) = w.script("scripts/cloud-onnxruntime.sh", &[], &envs);
    assert_eq!(code, 0, "{err}");
    let again = std::fs::read_to_string(cargo_home.join("config.toml")).unwrap();
    assert_eq!(again.matches("ORT_LIB_LOCATION").count(), 1, "{again}");
    let (code, _, _) = w.script("scripts/cloud-onnxruntime.sh", &["--check"], &envs);
    assert_eq!(code, 0);
    // The repository's own cargo config is never the place for this.
    assert!(!w.clone.join(".cargo").exists());
    // A foreign [env] table it did not write is left alone, with instructions.
    let foreign = w.home.join("foreign-cargo");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("config.toml"), "[env]\nFOO = \"bar\"\n").unwrap();
    let (code, _, err) = w.script(
        "scripts/cloud-onnxruntime.sh",
        &[],
        &[("CARGO_HOME", foreign.to_str().unwrap())],
    );
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("merge this into it by hand"), "{err}");
    assert_eq!(
        std::fs::read_to_string(foreign.join("config.toml")).unwrap(),
        "[env]\nFOO = \"bar\"\n"
    );
}

/// The queue's refs are compare-and-swap pushes outside `refs/heads/*`. A
/// remote that refuses them (a cloud session's git proxy) gets its refusal
/// reported verbatim: no request branch is published (the GitHub Actions relay
/// was removed, CEO decision 2026-11-06) and nothing is claimed.
#[test]
fn cloud_session_a_refused_queue_push_reports_the_refusal_and_publishes_no_request() {
    let w = World::new("refused");
    w.mark();
    let (code, _, err) = w.susi(&["workflow", "own-identity"], &[]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = w.susi(
        &[
            "tasks",
            "add",
            "refused work",
            "--accept",
            "cargo --version",
        ],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("added T-CLAUDECCRTEST-1"), "{out}");
    w.git(&w.clone, &["add", ".agents/tasks/T-CLAUDECCRTEST-1.json"]);
    w.git(&w.clone, &["commit", "--quiet", "-m", "task"]);
    let bare = w.bare();
    w.refuse_queue_pushes();
    let (code, _, err) = w.susi(
        &["tasks", "claim", "T-CLAUDECCRTEST-1", "--scope", "work"],
        &[],
    );
    assert_ne!(code, 0);
    assert!(err.contains("HTTP 403"), "{err}");
    assert!(w.git(&bare, &["for-each-ref", "refs/claims/"]).is_empty());
    assert_eq!(
        w.git(
            &bare,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"]
        ),
        "refs/heads/main"
    );
}

#[test]
fn cloud_session_parallel_workflow_sync_and_finish_run_in_the_clone() {
    let w = World::new("sync");
    let (code, _, err) = w.script("scripts/parallel-workflow.sh", &["sync"], &[]);
    assert_ne!(code, 0, "an unmarked primary checkout is refused");
    assert!(err.contains("Use your own linked worktree"), "{err}");

    w.mark();
    let (code, _, err) = w.script("scripts/parallel-workflow.sh", &["sync"], &[]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn cloud_session_settings_json_permissions() {
    let settings = std::fs::read_to_string(".claude/settings.json").unwrap();
    let json: serde_json::Value = serde_json::from_str(&settings).unwrap();

    let allow = json["permissions"]["allow"].as_array().unwrap();
    let deny = json["permissions"]["deny"].as_array().unwrap();

    let expected_allow = vec![
        "Bash(cargo*)",
        "Bash(git*)",
        "Bash(target/debug/susi*)",
        "Bash(susi*)",
    ];
    let expected_deny = vec![
        "mcp__claude-code-remote__request_computer_folder",
        "mcp__claude-code-remote__list_computer_folders",
        "mcp__claude-code-remote__confirm_repository_trust",
        "mcp__remote-devices*",
        "mcp__computer-use*",
        "mcp__Claude_Browser*",
        "mcp__claude-in-chrome*",
    ];

    for ea in expected_allow {
        assert!(
            allow.iter().any(|v| v.as_str().unwrap() == ea),
            "missing allow: {}",
            ea
        );
    }
    for ed in expected_deny {
        assert!(
            deny.iter().any(|v| v.as_str().unwrap() == ed),
            "missing deny: {}",
            ed
        );
    }
}
