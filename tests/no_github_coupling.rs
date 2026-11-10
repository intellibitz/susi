#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! GitHub is storage only (CEO mandate 2026-11-09 00:15 IST).
//!
//! The repositories may move to another git host at any time, so nothing in
//! the tree may depend on a GitHub feature: no `gh` invocation, no REST or
//! GraphQL call, no commit status, no release-asset download. Git itself
//! (`git push`, `git fetch`, tags, branches) is the only contact with the host,
//! and the remote URL lives in configuration (`git remote`, an environment
//! variable), never in code. Releases are tags plus committed records
//! (`releases/<tag>.json`, `docs/releases.md`); a landing's record is its gate
//! receipt (`.agents/receipts/`).
//!
//! This test scans every tracked file. The allow-list below is the only
//! exception, and each entry says why it may quote a retired form.

use std::path::Path;
use std::process::Command;

/// Tracked paths (prefixes) that may contain a pattern, and why.
const ALLOWED: [(&str, &str); 2] = [
    (
        ".agents/",
        "history: task records, evidence ledgers and receipts quote the commands \
         that ran when they were written; they are records, never executed",
    ),
    (
        "tests/no_github_coupling.rs",
        "this guard has to name the patterns it refuses",
    ),
];

/// `gh` subcommands that talk to GitHub. A shell line that runs any of them is
/// a coupling (`gh api`, `gh release download`, `gh run list`, ...).
const GH_SUBCOMMANDS: [&str; 18] = [
    "api",
    "attestation",
    "auth",
    "browse",
    "cache",
    "codespace",
    "gist",
    "issue",
    "label",
    "pr",
    "release",
    "repo",
    "ruleset",
    "run",
    "search",
    "secret",
    "variable",
    "workflow",
];

/// Literal markers of a GitHub API, release-page or registry dependency. Susi's
/// own images name no GitHub registry (`ghcr.io`); other projects' images
/// there, such as the GitHub MCP server, are user features, not coupling.
const LITERALS: [&str; 5] = [
    "api.github.com",
    "/statuses/",
    "releases/latest/download",
    "new(\"gh\")",
    "ghcr.io/intellibitz",
];

/// The `gh <subcommand>` invocation on `line`, if any: `gh` as a whole word
/// (at the start, or after whitespace, a quote or a shell operator) followed by
/// a GitHub subcommand.
fn gh_invocation(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(found) = line[from..].find("gh ") {
        let at = from + found;
        from = at + 3;
        let word_start = at == 0
            || matches!(
                bytes[at - 1],
                b' ' | b'\t' | b'"' | b'\'' | b'`' | b'(' | b'$' | b';' | b'&' | b'|' | b'{'
            );
        if !word_start {
            continue;
        }
        let rest = line[at + 3..].trim_start();
        let word: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if GH_SUBCOMMANDS.contains(&word.as_str()) {
            return Some(format!("gh {word}"));
        }
    }
    None
}

fn couplings(line: &str) -> Vec<String> {
    let mut found: Vec<String> = LITERALS
        .iter()
        .filter(|literal| line.contains(**literal))
        .map(|literal| (*literal).to_owned())
        .collect();
    found.extend(gh_invocation(line));
    found
}

fn tracked_files(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git ls-files runs");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn nothing_tracked_depends_on_a_github_feature() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    let mut scanned = 0usize;
    for path in tracked_files(root) {
        let normalized = path.replace('\\', "/");
        if ALLOWED
            .iter()
            .any(|(prefix, _)| normalized.starts_with(prefix))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(&path)) else {
            continue; // deleted in the working tree
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue; // binary
        };
        scanned += 1;
        for (number, line) in text.lines().enumerate() {
            for coupling in couplings(line) {
                hits.push(format!(
                    "{normalized}:{}: {coupling}: {}",
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        scanned > 100,
        "only {scanned} tracked text files were scanned"
    );
    assert!(
        hits.is_empty(),
        "GitHub is storage only (AGENTS.md, CEO mandate 2026-11-09): use git push/fetch, tags and committed records instead of:\n{}",
        hits.join("\n")
    );
}

/// GitHub-only integration files: the `.github/` directory (issue, pull-request
/// and discussion templates, Copilot instructions, workflows) and the
/// root-level files GitHub alone reads. CEO 2026-11-11: zero dependency on
/// GitHub, no GitHub hooks.
fn github_only_path(path: &str) -> bool {
    path.starts_with(".github/") || matches!(path, "CODEOWNERS" | "FUNDING.yml" | "dependabot.yml")
}

#[test]
fn no_github_hook_files_are_tracked() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let found: Vec<String> = tracked_files(root)
        .into_iter()
        .map(|path| path.replace('\\', "/"))
        .filter(|path| github_only_path(path))
        .collect();
    assert!(
        found.is_empty(),
        "GitHub-only integration files are not allowed (zero GitHub dependency): {found:?}"
    );
}

#[test]
fn github_only_path_matches_hook_files_and_nothing_else() {
    for hook in [
        ".github/workflows/test.yml",
        ".github/ISSUE_TEMPLATE/bug_report.md",
        ".github/copilot-instructions.md",
        "CODEOWNERS",
    ] {
        assert!(github_only_path(hook), "{hook} must be refused");
    }
    for ordinary in [
        ".githooks/pre-push",
        "docs/github.md",
        "vendor/x/.github-like",
    ] {
        assert!(
            !github_only_path(ordinary),
            "{ordinary} is not a GitHub hook"
        );
    }
}

#[test]
fn every_allow_list_entry_is_tracked_and_explained() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tracked = tracked_files(root);
    for (prefix, why) in ALLOWED {
        assert!(
            why.len() > 30,
            "{prefix}: say why it may quote a retired form"
        );
        assert!(
            tracked.iter().any(|path| path.starts_with(prefix)),
            "{prefix} is not tracked; drop it from the allow-list"
        );
    }
}

#[test]
fn the_matcher_finds_couplings_and_ignores_prose() {
    for line in [
        "gh api repos/o/r/statuses/abc",
        "    if ! gh release download \"$tag\" --repo x",
        "state=$(gh run list --workflow Test)",
        "echo x | gh api -X PUT repos/o/r/rulesets/1",
        "let mut command = Command::new(\"gh\");",
        "curl -L https://example.invalid/o/r/releases/latest/download/susi",
        "https://api.github.com/repos/o/r",
        "    image: ghcr.io/intellibitz/susi:latest",
    ] {
        assert!(!couplings(line).is_empty(), "missed: {line}");
    }
    for line in [
        "the agent walked through the high ground",
        "highest gh-pages",
        "laugh release",
        "\"gh\",",
        "git push origin vX.Y.Z",
        "git ls-remote --tags origin",
        "package: \"ghcr.io/github/github-mcp-server\"",
    ] {
        assert!(couplings(line).is_empty(), "false positive: {line}");
    }
}
