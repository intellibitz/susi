//! Source guard: no unattended console launch opens a window.
//!
//! The detached daemon has no console, so a console program it launches
//! (`icacls`, `tasklist`, `netstat`, `taskkill`, `nvidia-smi`) gets a new,
//! visible console window from Windows (CEO, 2026-11-09: error 0x800700e8
//! launching `icacls` on `node.key`). Owner-only ACLs are set and read through
//! `susi_vendor_winapi::acl`, which starts no process; every other helper
//! starts through `susi_paths::console::background_command`, which owns
//! `CREATE_NO_WINDOW`. This test fails when production source launches one of
//! them directly.

use std::fs;
use std::path::{Path, PathBuf};

/// Programs that must never be launched directly from production source.
const GUARDED: [&str; 5] = ["icacls", "tasklist", "netstat", "taskkill", "nvidia-smi"];

/// `icacls` is never launched at all (ACLs are native); the rest may only be
/// launched through the helper.
const NEVER_LAUNCHED: &str = "icacls";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            // Test trees, build output and vendored third-party source are not
            // production launch sites.
            if matches!(name.to_str(), Some("tests" | "target" | "vendor")) {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name != "tests.rs" && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
}

/// The production part of a source file: everything before its trailing
/// `#[cfg(test)]` module.
fn production_part(source: &str) -> &str {
    match source.find("\n#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// Production launches of a guarded program: `(file, line number, program)`.
fn direct_launches() -> Vec<(String, usize, &'static str)> {
    let mut files = Vec::new();
    for crate_dir in fs::read_dir(root().join("crates"))
        .expect("crates dir")
        .flatten()
    {
        rust_files(&crate_dir.path().join("src"), &mut files);
    }
    rust_files(&root().join("src"), &mut files);
    let mut found = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(root())
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        for (line, program) in scan_source(&source) {
            found.push((relative.clone(), line, program));
        }
    }
    found
}

/// Guarded launches in one file's production source: `(line, program)`.
fn scan_source(source: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (index, line) in production_part(source).lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for program in GUARDED {
            let quoted = format!("(\"{program}\")");
            let direct = line.contains(&format!("Command::new{quoted}"));
            let helper = line.contains(&format!("background_command{quoted}"));
            // `Command::new("x")` is a direct launch; the helper is the
            // sanctioned way, except for `icacls`, which is never launched.
            if direct || (helper && program == NEVER_LAUNCHED) {
                found.push((index + 1, program));
            }
        }
    }
    found
}

#[test]
fn no_production_source_launches_a_console_helper_directly() {
    let found = direct_launches();
    assert!(
        found.is_empty(),
        "unattended console launches must use susi_paths::console::background_command \
         (and `icacls` must not be launched at all; use susi_vendor_winapi::acl): {found:#?}"
    );
}

#[test]
fn the_helper_owns_create_no_window() {
    let console = fs::read_to_string(root().join("crates/susi-paths/src/console.rs"))
        .expect("console helper source");
    assert!(console.contains("pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;"));
    assert!(console.contains("creation_flags(CREATE_NO_WINDOW)"));
    // No other production file hardcodes the flag value.
    let mut files = Vec::new();
    for crate_dir in fs::read_dir(root().join("crates"))
        .expect("crates dir")
        .flatten()
    {
        rust_files(&crate_dir.path().join("src"), &mut files);
    }
    let strays: Vec<String> = files
        .iter()
        .filter(|f| !f.ends_with("susi-paths/src/console.rs"))
        .filter(|f| {
            fs::read_to_string(f).is_ok_and(|s| {
                production_part(&s)
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| l.contains("0x0800_0000") || l.contains("0x08000000"))
            })
        })
        .map(|f| f.display().to_string())
        .collect();
    assert!(
        strays.is_empty(),
        "CREATE_NO_WINDOW belongs to susi_paths::console only: {strays:?}"
    );
}

#[test]
fn the_guard_sees_what_it_must_and_only_that() {
    let bad = "fn a() {\n    let _ = std::process::Command::new(\"taskkill\").status();\n}\n";
    assert_eq!(scan_source(bad), vec![(2, "taskkill")]);
    let icacls = "let _ = susi_paths::console::background_command(\"icacls\");";
    assert_eq!(scan_source(icacls), vec![(1, "icacls")]);
    let good = "let _ = susi_paths::console::background_command(\"taskkill\");\n\
                // Command::new(\"netstat\")\n\
                #[cfg(test)]\n\
                mod t { fn x() { Command::new(\"icacls\"); } }\n";
    assert!(scan_source(good).is_empty());
}
