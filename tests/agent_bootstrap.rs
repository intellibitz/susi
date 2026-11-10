#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Current agent-directive drift guards, and the session-start script's
//! promise never to block a session. The worktree start itself
//! (`scripts/susi-worktree.sh`, which execs `workflow start`) is Susi
//! Commercial Apps' dev workflow since the lean-core move and is tested there.

use std::path::Path;
use std::process::Command;

mod common;
use common::script_command;

/// Run a script in `dir` with `envs`; (code, stdout, stderr).
fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = script_command(Path::new(program));
    cmd.args(args).current_dir(dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = common::run_script_bounded(&mut cmd, b"", common::SCRIPT_BOUND);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_agent_tool_auto_loads_the_same_first_step() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = ["CLAUDE.md", "GEMINI.md"];
    let canonical = std::fs::read_to_string(root.join(files[0])).unwrap();
    for file in files {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        assert_eq!(
            text, canonical,
            "{file} diverged from the canonical pointer"
        );
        for required in ["AGENTS.md", "Mandates 62–72", "CEO never inspects code"] {
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
    // CLAUDE.md, GEMINI.md and the tracked tool directories; the Copilot file
    // left with the .github/ directory (T-CLAUDENOGITHUB-1).
    assert!(
        inspected >= 3,
        "the known agent entrypoints must be inspected"
    );
}
