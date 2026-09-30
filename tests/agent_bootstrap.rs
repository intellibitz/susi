#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! An agent lands in its own, ready-to-work worktree without anyone telling it
//! how: `scripts/susi-worktree.sh` (no name needed) and `susi workflow start`
//! do the whole setup, and every agent tool's auto-loaded file points there.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_AGENT");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

struct World {
    root: PathBuf,
    primary: PathBuf,
}

impl World {
    /// bare server + a primary clone carrying the real scripts, parked on a
    /// stale branch (as the real primary checkout was).
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-boot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("susi");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for s in ["susi-worktree.sh", "park-primary.sh", "setup-dev.sh"] {
            std::fs::copy(
                repo.join("scripts").join(s),
                primary.join("scripts").join(s),
            )
            .unwrap();
        }
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["switch", "--quiet", "-c", "stale-branch"]);
        // origin/main moves on after the primary checkout was left behind.
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
        git(
            &other,
            &["commit", "--allow-empty", "--quiet", "-m", "newer"],
        );
        git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        Self { root, primary }
    }

    fn worktree(&self, args: &[&str], agent: &str) -> (i32, String, String) {
        run(
            &self.primary,
            self.primary
                .join("scripts/susi-worktree.sh")
                .to_str()
                .unwrap(),
            args,
            &[("SUSI_AGENT", agent)],
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cd_target(stdout: &str) -> PathBuf {
    let last = stdout.lines().last().unwrap();
    PathBuf::from(
        last.strip_prefix("cd ")
            .unwrap_or_else(|| panic!("last line must be `cd <path>`: {stdout}")),
    )
}

#[test]
fn no_name_needed_the_agent_gets_a_ready_worktree() {
    let w = World::new("auto");
    let (code, out, err) = w.worktree(&[], "Codex Agent");
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(dest.is_dir(), "{dest:?}");
    // Auto name is <agent>-<timestamp>, branched from the latest origin/main.
    let branch = git(&dest, &["symbolic-ref", "--short", "HEAD"]);
    assert!(branch.starts_with("codex-agent-"), "{branch}");
    assert_eq!(
        git(&dest, &["rev-parse", "HEAD"]),
        git(&w.primary, &["rev-parse", "origin/main"])
    );
    // Hooks are installed for the worktree without the agent asking.
    assert_eq!(
        git(&dest, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    // The primary checkout was parked: on main at origin/main, off the stale branch.
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    assert_eq!(
        git(&w.primary, &["rev-parse", "HEAD"]),
        git(&w.primary, &["rev-parse", "origin/main"])
    );
}

#[test]
fn a_named_worktree_is_reused_not_duplicated() {
    let w = World::new("named");
    let (code, out, err) = w.worktree(&["my-feature"], "claude");
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(dest.ends_with("susi-my-feature"), "{dest:?}");
    let (code, out2, err2) = w.worktree(&["my-feature"], "claude");
    assert_eq!(code, 0, "{err2}");
    assert_eq!(cd_target(&out2), dest);
    assert!(err2.contains("already exists"), "{err2}");
}

#[test]
fn main_and_head_are_not_valid_branch_names() {
    let w = World::new("badname");
    for bad in ["main", "HEAD"] {
        let (code, _, err) = w.worktree(&[bad], "claude");
        assert_eq!(code, 2, "{bad}");
        assert!(err.contains("refusing branch name"), "{err}");
    }
}

#[test]
fn susi_workflow_start_wraps_the_same_setup() {
    let w = World::new("cli");
    let home = w.root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (code, out, err) = run(
        &w.primary,
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "start", "cli-made"],
        &[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", home.join("xdg").to_str().unwrap()),
            ("SUSI_AGENT", "claude"),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(dest.ends_with("susi-cli-made"), "{dest:?}");
    assert_eq!(git(&dest, &["symbolic-ref", "--short", "HEAD"]), "cli-made");
}

#[test]
fn every_agent_tool_auto_loads_the_same_first_step() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "CLAUDE.md",
        "GEMINI.md",
        ".github/copilot-instructions.md",
        ".cursor/rules/susi-workflow.mdc",
        ".devin/rules/susi-workflow.md",
    ] {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|_| panic!("{file} missing"));
        for needle in [
            "susi workflow check",
            "scripts/susi-worktree.sh",
            "Task: <id>",
            "AGENTS.md",
        ] {
            assert!(text.contains(needle), "{file} must mention `{needle}`");
        }
    }
    let cursor = std::fs::read_to_string(root.join(".cursor/rules/susi-workflow.mdc")).unwrap();
    assert!(
        cursor.contains("alwaysApply: true"),
        "Cursor must always apply the rule"
    );
    let devin = std::fs::read_to_string(root.join(".devin/rules/susi-workflow.md")).unwrap();
    assert!(
        devin.contains("trigger: always_on"),
        "Devin must always apply the rule"
    );
    // Claude Code and Codex run the check at session start via a "hooks" key.
    for hook_file in [".claude/settings.json", ".codex/hooks.json"] {
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(hook_file)).unwrap()).unwrap();
        let cmd = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("scripts/workflow-session-start.sh"), "{cmd}");
    }
    // Devin CLI: in hooks.v1.json the hooks object is the whole file.
    let devin_hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".devin/hooks.v1.json")).unwrap())
            .unwrap();
    let cmd = devin_hooks["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.contains("scripts/workflow-session-start.sh"), "{cmd}");
    assert!(root.join("scripts/workflow-session-start.sh").is_file());
}

#[test]
fn the_session_start_hook_never_blocks_a_session() {
    // Outside any repo and with no susi on PATH it still exits 0 and explains.
    let dir = std::env::temp_dir().join(format!("susi-boot-hook-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/workflow-session-start.sh");
    let (code, out, _) = run(
        &dir,
        script.to_str().unwrap(),
        &[],
        &[
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CEILING_DIRECTORIES", "/tmp"),
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("susi-worktree.sh"), "{out}");
}

#[test]
fn simultaneous_workers_get_distinct_worktrees_and_local_identities() {
    let w = World::new("parallel-start");
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| w.worktree(&[], "shared-login"));
        let right = scope.spawn(|| w.worktree(&[], "shared-login"));
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_eq!(left.0, 0, "{}", left.2);
    assert_eq!(right.0, 0, "{}", right.2);
    let a = cd_target(&left.1);
    let b = cd_target(&right.1);
    assert_ne!(a, b);
    assert_ne!(
        git(&a, &["config", "--worktree", "--get", "susi.agent"]),
        git(&b, &["config", "--worktree", "--get", "susi.agent"])
    );
}
