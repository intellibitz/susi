//! A cloud session's clone is the worker's own worktree.
//!
//! A cloud session (Claude Code on the web, or any agent harness that clones
//! the repository into a disposable container) has no laptop behind it: no
//! shared primary checkout to keep parked, no sibling worktrees, no watcher,
//! and nothing to converge once the container is reclaimed. The clone the
//! harness made, on the branch it checked out, *is* the worker's worktree in
//! the sense of Mandate 49, and everything the loop needs — build, test,
//! claim, push — runs inside the container. It never asks for the operator's
//! machine (Remote Control, a computer folder, a button to press).
//!
//! The fact is recorded on the clone, in its git config, by
//! `scripts/cloud-session.sh --mark` (the session-start hook runs it for the
//! clone it was started for), and every consumer reads that one record:
//!
//! * [`MARK_KEY`] (`susi.cloud = true`): this clone is a cloud session's;
//! * [`TOOL_KEY`] (`susi.cloudTool = CLAUDE`): the agent tool the harness runs,
//!   which prefixes the worker's identity (`CLAUDE` + the branch).
//!
//! The harness's own environment variables are deliberately *not* read here:
//! a test binary run inside a cloud container inherits them, and a fixture
//! repository must behave as it does on a laptop. The one variable this module
//! honours is [`EXPLICIT_ENV`] (`SUSI_CLOUD_SESSION=1` or `0`), the operator's
//! or a test's explicit word, which beats the recorded mark either way.
//!
//! `scripts/cloud-session.sh` is the shell side of the same rule; the two must
//! agree (`tests/cloud_session.rs` pins it).

use std::path::Path;
use std::process::Command;

/// The git config key `scripts/cloud-session.sh --mark` sets on a cloud
/// session's clone.
pub const MARK_KEY: &str = "susi.cloud";
/// The git config key naming the agent tool the cloud harness runs.
pub const TOOL_KEY: &str = "susi.cloudTool";
/// The environment variable that states the answer explicitly: `1`, `true`,
/// `yes` or `on` for a cloud session, `0`, `false`, `no` or `off` for none.
pub const EXPLICIT_ENV: &str = "SUSI_CLOUD_SESSION";

/// What an explicit [`EXPLICIT_ENV`] value says: `Some(true)` for a cloud
/// session, `Some(false)` for none, `None` when nothing (or nonsense) is said.
#[must_use]
pub fn explicit(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on") => Some(true),
        Some("0" | "false" | "no" | "off") => Some(false),
        Some(_) | None => None,
    }
}

/// The pure decision: the explicit word first, then the clone's mark.
#[must_use]
pub fn decide(explicit_value: Option<&str>, marked: bool) -> bool {
    explicit(explicit_value).unwrap_or(marked)
}

/// The token of a tool name, as identities are spelled: upper-case letters and
/// digits only; `None` when nothing is left.
fn token(raw: &str) -> Option<String> {
    let t: String = raw
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (!t.is_empty()).then_some(t)
}

/// A git invocation that sees the repository it is pointed at, not the one a
/// calling hook exported through `GIT_DIR`.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether the clone at (or above) `root` carries the cloud mark.
#[must_use]
pub fn marked(root: &Path) -> bool {
    git(root, &["config", "--get", "--bool", MARK_KEY]).as_deref() == Some("true")
}

/// Is `root`'s clone a cloud session's own worktree?
#[must_use]
pub fn is_cloud(root: &Path) -> bool {
    let explicit_value = std::env::var(EXPLICIT_ENV).ok();
    decide(explicit_value.as_deref(), marked(root))
}

/// The agent tool the clone was marked with, as an identity prefix (`CLAUDE`),
/// or `None` when the mark names none.
#[must_use]
pub fn tool(root: &Path) -> Option<String> {
    git(root, &["config", "--get", TOOL_KEY])
        .as_deref()
        .and_then(token)
}

#[cfg(test)]
mod cloud_session_tests {
    use super::*;

    #[test]
    fn cloud_session_explicit_word_is_read_both_ways() {
        for yes in ["1", "true", "YES", " on "] {
            assert_eq!(explicit(Some(yes)), Some(true), "{yes}");
        }
        for no in ["0", "false", "No", "off"] {
            assert_eq!(explicit(Some(no)), Some(false), "{no}");
        }
        for nothing in ["", "maybe", "2"] {
            assert_eq!(explicit(Some(nothing)), None, "{nothing:?}");
        }
        assert_eq!(explicit(None), None);
    }

    #[test]
    fn cloud_session_explicit_word_beats_the_mark() {
        // Nothing said: the mark decides.
        assert!(decide(None, true));
        assert!(!decide(None, false));
        assert!(!decide(Some("maybe"), false));
        // Said: the word decides, whatever the clone recorded.
        assert!(decide(Some("1"), false));
        assert!(!decide(Some("0"), true));
    }

    #[test]
    fn cloud_session_tool_is_an_identity_token() {
        assert_eq!(token("claude"), Some("CLAUDE".into()));
        assert_eq!(token("Claude-Code 2"), Some("CLAUDECODE2".into()));
        assert_eq!(token("--"), None);
    }
}
