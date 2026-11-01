#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Current agent-directive drift guards plus legacy worktree-script regression
//! coverage. The scripts remain compatibility behavior; automatic agent startup
//! follows the native local contract in AGENTS.md.

use std::path::{Path, PathBuf};
use std::process::Command;
use susi_paths::test_env::isolate_home;

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

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    run_as(dir, None, program, args, envs)
}

/// [`run`], with `home` as the child's whole home when one is given.
fn run_as(
    dir: &Path,
    home: Option<&Path>,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (i32, String, String) {
    let mut cmd = if program.ends_with(".sh") {
        script_command(Path::new(program))
    } else {
        Command::new(program)
    };
    if let Some(home) = home {
        isolate_home(&mut cmd, home);
    }
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        // scripts/susi-worktree.sh parks the primary through
        // scripts/park-primary.sh, the stub that execs `susi workflow
        // park-primary`: the binary under test.
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
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
        for s in [
            "susi-worktree.sh",
            "park-primary.sh",
            "setup-dev.sh",
            "posix-compat.sh",
        ] {
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
    let (code, out, err) = run_as(
        &w.primary,
        Some(&home),
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "start", "cli-made"],
        &[
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
    let files = ["CLAUDE.md", "GEMINI.md", ".github/copilot-instructions.md"];
    let canonical = std::fs::read_to_string(root.join(files[0])).unwrap();
    for file in files {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        assert_eq!(
            text, canonical,
            "{file} diverged from the canonical pointer"
        );
        for required in ["AGENTS.md", "Mandates 62–71", "CEO never inspects code"] {
            assert!(text.contains(required), "{file} must carry {required}");
        }
        for forbidden in [
            "workflow-session-start.sh",
            "scripts/seed-target.sh",
            "susi tasks claim",
            "git fetch",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} reinstated legacy bootstrap: {forbidden}"
            );
        }
    }
    // No tool may silently run the old shell/network bootstrap before reading
    // the canonical local contract. Useful per-tool permissions remain intact.
    for hook_file in [".claude/settings.json", ".codex/hooks.json"] {
        if let Ok(text) = std::fs::read_to_string(root.join(hook_file)) {
            let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert!(
                settings
                    .get("hooks")
                    .is_none_or(|h| h.as_object().is_some_and(|h| h.is_empty())),
                "{hook_file} contains unreviewed automatic startup hooks"
            );
        }
    }
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

/// `susi workflow start` as the binary, with the stub out of the way: the
/// same worktree, the same identity, the same parked primary, the same
/// refusals with the script's exit codes, and `cd <path>` last on stdout.
#[test]
fn the_binary_starts_a_worktree_as_the_script_did() {
    let w = World::new("binary");
    let susi = env!("CARGO_BIN_EXE_susi");
    let envs = [
        ("SUSI_AGENT", "Codex Agent"),
        ("SUSI_PRIMARY_WATCH_DISABLE", "1"),
    ];
    let (code, out, err) = run(&w.primary, susi, &["workflow", "start"], &envs);
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(dest.is_dir(), "{dest:?}");
    let branch = git(&dest, &["symbolic-ref", "--short", "HEAD"]);
    assert!(branch.starts_with("codex-agent-"), "{branch}");
    // The script's date form, with the pid last: <slug>-<yyyymmdd>-<hhmmss>-<pid>.
    let tail = branch.strip_prefix("codex-agent-").unwrap();
    let parts: Vec<&str> = tail.split('-').collect();
    assert_eq!(parts.len(), 3, "{branch}");
    assert!(
        parts[0].len() == 8 && parts[0].bytes().all(|b| b.is_ascii_digit()),
        "{branch}"
    );
    assert!(
        parts[1].len() == 6 && parts[1].bytes().all(|b| b.is_ascii_digit()),
        "{branch}"
    );
    assert!(parts[2].parse::<u64>().is_ok(), "{branch}");
    assert_eq!(
        git(&dest, &["config", "--worktree", "--get", "susi.agent"]),
        "CODEXAGENT".to_string() + &parts.join("").to_ascii_uppercase()
    );
    assert_eq!(
        git(&dest, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    assert!(err.contains("worktree:"), "{err}");
    // The primary checkout was parked.
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
    // A base other than origin/main.
    let (code, out, err) = run(
        &w.primary,
        susi,
        &["workflow", "start", "from-first", "origin/main~1"],
        &envs,
    );
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert_eq!(
        git(&dest, &["rev-parse", "HEAD"]),
        git(&w.primary, &["rev-parse", "origin/main~1"])
    );
    // The script's exit codes: 2 for a name that is no branch name.
    let (code, _, err) = run(&w.primary, susi, &["workflow", "start", "HEAD"], &envs);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("refusing branch name"), "{err}");
}

/// Two starts at once never share a directory, a token or a branch: the
/// start lock serialises them, and each gets its own name.
#[test]
fn simultaneous_binary_starts_are_serialised_and_distinct() {
    let w = World::new("binary-parallel");
    let susi = env!("CARGO_BIN_EXE_susi");
    let envs = [
        ("SUSI_AGENT", "shared"),
        ("SUSI_PRIMARY_WATCH_DISABLE", "1"),
    ];
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| run(&w.primary, susi, &["workflow", "start"], &envs));
        let right = scope.spawn(|| run(&w.primary, susi, &["workflow", "start"], &envs));
        (left.join().unwrap(), right.join().unwrap())
    });
    assert_eq!(left.0, 0, "{}", left.2);
    assert_eq!(right.0, 0, "{}", right.2);
    let (a, b) = (cd_target(&left.1), cd_target(&right.1));
    assert_ne!(a, b, "two workers must never share a directory");
    assert_ne!(
        git(&a, &["config", "--worktree", "--get", "susi.agent"]),
        git(&b, &["config", "--worktree", "--get", "susi.agent"])
    );
    let common = git(
        &w.primary,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert!(
        !Path::new(&common)
            .join("susi-worktree-start.lockdir")
            .exists(),
        "the start lock is let go of after"
    );
}

/// On Windows an installed `susi.exe` makes a worktree with no Git userland
/// on PATH: only Git's `cmd` (git itself), System32 and the Windows
/// directory. The bash version needed `flock`, `awk`, `sed`, `tr` and
/// `cygpath` from Git's `usr/bin`. Returns at once where Git for Windows is
/// not installed.
#[test]
fn the_start_needs_no_git_userland_on_windows() {
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
    let (code, out, err) = run(
        &w.primary,
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "start", "bare-path"],
        &[
            ("PATH", path.to_str().unwrap()),
            ("SUSI_AGENT", "claude"),
            ("SUSI_PRIMARY_WATCH_DISABLE", "1"),
        ],
    );
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(dest.is_dir(), "{dest:?}");
    assert_eq!(
        git(&dest, &["symbolic-ref", "--short", "HEAD"]),
        "bare-path"
    );
    assert_eq!(
        git(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main",
        "the primary was parked without a shell"
    );
}

/// Discover tracked tool directives so adding a new agent cannot quietly bring
/// back the retired shell/network bootstrap or a divergent policy document.
#[test]
fn tracked_agent_directives_do_not_reintroduce_legacy_bootstrap() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let mut inspected = 0;
    for bytes in listed.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let path = std::str::from_utf8(bytes).unwrap();
        let tool_directory = [
            ".claude/",
            ".codex/",
            ".cursor/",
            ".gemini/",
            ".windsurf/",
            ".roo/",
            ".kilocode/",
            ".kiro/",
            ".junie/",
            ".opencode/",
            ".openhands/",
            ".continue/",
            ".github/instructions/",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix));
        let tool_file = [
            "CLAUDE.md",
            "GEMINI.md",
            ".github/copilot-instructions.md",
            ".cursorrules",
            ".windsurfrules",
            ".clinerules",
            ".aider.conf.yml",
        ]
        .contains(&path);
        if !tool_directory && !tool_file {
            continue;
        }
        // A deleted tracked file is absent before the commit; its removal is
        // intentional, not a reason to resurrect its obsolete directive.
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            assert!(
                !root.join(path).exists(),
                "unreadable tracked directive {path}"
            );
            continue;
        };
        inspected += 1;
        for retired in [
            "workflow-session-start.sh",
            "scripts/seed-target.sh",
            "scripts/susi-worktree.sh",
            "git fetch origin",
        ] {
            assert!(
                !text.contains(retired),
                "{path} reinstated retired bootstrap: {retired}"
            );
        }
        if path.ends_with(".md") || path.ends_with(".mdc") || tool_file {
            assert!(
                text.contains("AGENTS.md"),
                "{path} must reference the canonical contract"
            );
        }
    }
    assert!(
        inspected >= 4,
        "the known agent entrypoints must be inspected"
    );
}
