#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

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

use std::path::{Path, PathBuf};
use std::process::Command;

const HOOKS: [&str; 4] = ["commit-msg", "pre-commit", "pre-push", "workflow-guard"];
const SCRIPTS: [&str; 12] = [
    "cloud-session.sh",
    "cloud-onnxruntime.sh",
    "check-worker-identity.sh",
    "check-task-scope.py",
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
/// hooks' escape hatches, and the `GIT_DIR` a calling hook exports.
const SCRUBBED: [&str; 12] = [
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
        cmd.current_dir(dir)
            .env("HOME", &self.home)
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

#[cfg(target_os = "linux")]
#[test]
fn cloud_onnxruntime_points_cargo_at_the_release_without_touching_the_repository() {
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
        "Bash(gh api*)",
        "Bash(scripts/*)",
        "Bash(.githooks/*)",
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
