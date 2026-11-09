//! Source guard: no unattended console launch opens a window.
//!
//! The detached daemon has no console, so a console program it launches
//! (`icacls`, `tasklist`, `netstat`, `taskkill`, `nvidia-smi`) gets a new,
//! visible console window from Windows (CEO, 2026-11-09: error 0x800700e8
//! launching `icacls` on `node.key`). Owner-only ACLs are set and read through
//! `susi_vendor_winapi::acl`, which starts no process; every other helper
//! starts through `susi_paths::console::background_command`, which owns
//! `CREATE_NO_WINDOW`.
//!
//! The guard reads every first-party Rust source in the repository: crate
//! sources (including their inline test modules and `tests.rs` files), crate
//! and root integration tests, `xtask`, `fuzz` and the root `build.rs`. Test
//! code is not exempt: a test fixture that runs `taskkill` while the gate is
//! detached opens the same window (the original `icacls` popup came from a
//! fixture helper). It fails when any of them launches a guarded program
//! directly, whatever the spacing, line breaks, case or `.exe` suffix.
//!
//! Two launches sit in files reserved by other native workers; this task may
//! not edit them. They are named in [`HELD_BY_OTHER_CLAIMS`] with their exact
//! count: the guard still fails if either grows or any new one appears, and it
//! also fails once a holder fixes theirs, so the entry must then be deleted.
//! The list may only shrink.

// Test code: a missing source file must fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Programs that must never be launched directly from first-party source.
const GUARDED: [&str; 5] = ["icacls", "tasklist", "netstat", "taskkill", "nvidia-smi"];

/// `icacls` is never launched at all (ACLs are native); the rest may only be
/// launched through the helper.
const NEVER_LAUNCHED: &str = "icacls";

/// Known direct launches in files another native worker holds:
/// `(file, program, count, holder)`.
const HELD_BY_OTHER_CLAIMS: [(&str, &str, usize, &str); 2] = [
    // Five `taskkill` calls in test teardown, reserved by the wintests claim.
    ("tests/gmcp_requirements.rs", "taskkill", 5, "wintests"),
    // The `tasklist` liveness probe, reserved by the dot-proof claim.
    (
        "xtask/src/bin/local-workflow.rs",
        "tasklist",
        1,
        "dot-proof",
    ),
];

/// This file spells every guarded program in its own fixtures.
const SELF_PATH: &str = "tests/no_console_window.rs";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every first-party `.rs` file under `dir`, build output and vendored
/// third-party source excluded.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if matches!(name.to_str(), Some("target" | "vendor" | ".git")) {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && relative(&path) != SELF_PATH {
            out.push(path);
        }
    }
}

fn all_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&root(), &mut files);
    files
        .iter()
        .filter_map(|file| Some((relative(file), fs::read_to_string(file).ok()?)))
        .collect()
}

/// `program` as a launcher sees it: lower case, without a `.exe` suffix.
fn normalized(program: &str) -> String {
    let lower = program.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Guarded launches in one file: `(line, program)`. A launch is
/// `Command::new` (any path prefix) or `background_command` whose first
/// argument is a string literal naming a guarded program; whitespace and line
/// breaks between the pieces do not matter. Full-line comments are skipped.
fn scan_source(source: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for call in ["Command::new", "background_command"] {
        for (at, _) in source.match_indices(call) {
            let line_start = source[..at].rfind('\n').map_or(0, |n| n + 1);
            if source[line_start..at].trim_start().starts_with("//") {
                continue;
            }
            let mut rest = source[at + call.len()..].trim_start();
            let Some(after_paren) = rest.strip_prefix('(') else {
                continue;
            };
            rest = after_paren.trim_start();
            rest = rest.strip_prefix('&').unwrap_or(rest).trim_start();
            let Some(literal) = rest.strip_prefix('"') else {
                continue;
            };
            let Some(end) = literal.find('"') else {
                continue;
            };
            let name = normalized(&literal[..end]);
            let Some(program) = GUARDED.iter().find(|p| **p == name) else {
                continue;
            };
            let direct = call == "Command::new";
            // `Command::new("x")` is a direct launch; the helper is the
            // sanctioned way, except for `icacls`, which is never launched.
            if direct || *program == NEVER_LAUNCHED {
                let line = source[..at].matches('\n').count() + 1;
                found.push((line, *program));
            }
        }
    }
    found.sort_unstable();
    found
}

/// Every guarded launch in the repository: `(file, line, program)`.
fn direct_launches() -> Vec<(String, usize, &'static str)> {
    let mut found = Vec::new();
    for (file, source) in all_sources() {
        for (line, program) in scan_source(&source) {
            found.push((file.clone(), line, program));
        }
    }
    found
}

#[test]
fn no_source_launches_a_console_helper_directly() {
    let found = direct_launches();
    let mut unexpected = Vec::new();
    for (file, line, program) in &found {
        let held = HELD_BY_OTHER_CLAIMS
            .iter()
            .any(|(f, p, _, _)| f == file && p == program);
        if !held {
            unexpected.push((file.as_str(), *line, *program));
        }
    }
    assert!(
        unexpected.is_empty(),
        "unattended console launches must use susi_paths::console::background_command \
         (and `icacls` must not be launched at all; use susi_vendor_winapi::acl): {unexpected:#?}"
    );
}

#[test]
fn the_launches_held_by_other_claims_are_exactly_the_recorded_ones() {
    let found = direct_launches();
    for (file, program, count, holder) in HELD_BY_OTHER_CLAIMS {
        let actual = found
            .iter()
            .filter(|(f, _, p)| f == file && *p == program)
            .count();
        assert_eq!(
            actual, count,
            "{file} (held by the {holder} claim) launches `{program}` directly {actual} time(s), \
             the guard records {count}: a holder that fixed theirs deletes the entry, and no \
             new launch is ever added here"
        );
    }
}

#[test]
fn the_helper_owns_create_no_window() {
    let console = fs::read_to_string(root().join("crates/susi-paths/src/console.rs"))
        .expect("console helper source");
    assert!(console.contains("pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;"));
    assert!(console.contains("creation_flags(CREATE_NO_WINDOW)"));
    // No other first-party production source hardcodes the flag value.
    let strays: Vec<String> = all_sources()
        .into_iter()
        .filter(|(file, _)| file != "crates/susi-paths/src/console.rs")
        .filter(|(file, _)| {
            !file.starts_with("tests/")
                && !file.contains("/tests/")
                && !file.ends_with("/tests.rs")
                && !file.ends_with("_tests.rs")
        })
        .filter(|(_, source)| {
            let production = match source.find("\n#[cfg(test)]") {
                Some(at) => &source[..at],
                None => source,
            };
            production
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains("0x0800_0000") || l.contains("0x08000000"))
        })
        .map(|(file, _)| file)
        .collect();
    assert!(
        strays.is_empty(),
        "CREATE_NO_WINDOW belongs to susi_paths::console only: {strays:?}"
    );
}

#[test]
fn the_scan_reaches_test_build_and_tool_sources() {
    let files: Vec<String> = all_sources().into_iter().map(|(file, _)| file).collect();
    for expected in [
        "build.rs",
        "tests/installed_windows_entry.rs",
        "xtask/src/main.rs",
        "crates/susi-paths/src/console.rs",
        "crates/susi-daemon/src/runtime_admin.rs",
    ] {
        assert!(
            files.iter().any(|f| f == expected),
            "{expected} must be scanned"
        );
    }
    assert!(
        files.iter().any(|f| f.contains("/tests/")),
        "crate integration tests must be scanned"
    );
    assert!(
        !files
            .iter()
            .any(|f| f.starts_with("target/") || f.starts_with("vendor/")),
        "build output and vendored source are not first-party"
    );
}

#[test]
fn the_guard_sees_what_it_must_and_only_that() {
    // A plain direct launch.
    let bad = "fn a() {\n    let _ = std::process::Command::new(\"taskkill\").status();\n}\n";
    assert_eq!(scan_source(bad), vec![(2, "taskkill")]);
    // Line breaks, a reference, upper case and a suffix do not hide it.
    let split = "let _ = Command::new(\n    &\"NetStat.EXE\"\n);";
    assert_eq!(scan_source(split), vec![(1, "netstat")]);
    let tokio = "tokio::process::Command::new  ( \"tasklist\" )";
    assert_eq!(scan_source(tokio), vec![(1, "tasklist")]);
    // `icacls` is never launched, helper or not.
    let icacls = "let _ = susi_paths::console::background_command(\"icacls\");";
    assert_eq!(scan_source(icacls), vec![(1, "icacls")]);
    let direct_icacls = "Command::new(\"icacls\")";
    assert_eq!(scan_source(direct_icacls), vec![(1, "icacls")]);
    // The helper, comments and unguarded programs are fine, in test modules
    // too (the scan has no test-module exemption, so a fixture is held to the
    // same rule as production).
    let good = "let _ = susi_paths::console::background_command(\"taskkill\");\n\
                // Command::new(\"netstat\")\n\
                let _ = Command::new(\"git\");\n\
                let _ = Command::new(program);\n\
                let _ = Command::new(\"cmd\");\n";
    assert!(scan_source(good).is_empty());
    let in_test_module = "#[cfg(test)]\nmod t { fn x() { Command::new(\"nvidia-smi\"); } }\n";
    assert_eq!(scan_source(in_test_module), vec![(2, "nvidia-smi")]);
}
