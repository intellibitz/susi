#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Build scripts are not production runtime modules: a failed invariant must
// abort the build loudly, so panic-on-error is the intended failure mode here.
#![allow(missing_docs)]
//! Root package build script: native link policy and agent-ledger version sync.
use serde_json::Value;
use std::env;
use std::fs;
use std::path::Path;

// The one `CREATE_NO_WINDOW` owner, compiled here from its single source: a
// build script run by a windowless supervisor must not open a console window
// for `nvidia-smi` (and a build dependency on `susi-paths` would put the whole
// crate in every build graph for one probe).
#[allow(dead_code)] // only `background_command` is used by this script
#[path = "crates/susi-paths/src/console.rs"]
mod console;

fn main() {
    susi_build_os_ucrt::configure_all_bins()
        .expect("Windows OS-UCRT boundary configuration failed");
    enable_repository_hooks();

    let cargo_toml = fs::read_to_string("Cargo.toml").expect("Missing Cargo.toml");
    let version = cargo_toml
        .lines()
        .find(|l| l.trim().starts_with("version = \""))
        .and_then(|l| l.split('"').nth(1))
        .expect("Could not find version in Cargo.toml")
        .to_string();

    for (rel, schema) in [
        (".agents/identity.json", "susi/identity/v1"),
        (".agents/roadmap.json", "susi/roadmap/v1"),
    ] {
        sync_json_version(Path::new(rel), schema, &version);
        println!("cargo:rerun-if-changed={rel}");
    }

    sync_readme_badge(&version);
    println!("cargo:rerun-if-changed=README.md");
    println!("cargo:rerun-if-changed=Cargo.toml");
}

/// Git never enables a repository's hooks on clone, so a fresh clone or a
/// cloud session would publish without `push-policy` (no force, no deletion,
/// real ancestry) until someone remembered `scripts/setup-dev.sh`. Every agent
/// builds before it publishes, so the first build of a clone turns the tracked
/// hooks on. It only fills an unset `core.hooksPath`: a deliberate value is
/// kept, a source tree without Git is skipped, and `SUSI_NO_HOOK_SETUP=1` opts
/// out. Best effort: a failure here never fails a build.
fn enable_repository_hooks() {
    println!("cargo:rerun-if-env-changed=SUSI_NO_HOOK_SETUP");
    if env::var_os("SUSI_NO_HOOK_SETUP").is_some_and(|value| value == "1")
        || !Path::new(".git").exists()
        || !Path::new(".githooks/pre-push").is_file()
    {
        return;
    }
    let configured = console::background_command("git")
        .args(["config", "--local", "--get", "core.hooksPath"])
        .output();
    let Ok(configured) = configured else {
        return;
    };
    // Exit 1 with no output is Git's "not set"; anything else is left alone.
    if configured.status.code() != Some(1) || !configured.stdout.is_empty() {
        return;
    }
    let enabled = console::background_command("git")
        .args(["config", "--local", "core.hooksPath", ".githooks"])
        .status()
        .is_ok_and(|status| status.success());
    if enabled {
        println!("cargo:warning=enabled this clone's Git hooks (core.hooksPath=.githooks): push-policy now guards every push");
    }
}

fn sync_json_version(path: &Path, expected_schema: &str, version: &str) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&raw) else {
        panic!("Invalid JSON {}", path.display());
    };
    let schema = doc.get("schema").and_then(|v| v.as_str()).unwrap_or("");
    if schema != expected_schema {
        panic!(
            "{}: expected schema {expected_schema:?}, got {schema:?}",
            path.display()
        );
    }
    if doc.get("version").and_then(|v| v.as_str()) != Some(version) {
        doc["version"] = Value::String(version.to_string());
        let pretty = serde_json::to_string_pretty(&doc).expect("serialize");
        write_atomic(path, &(pretty + "\n")).expect("write synced version");
    }
}

fn sync_readme_badge(version: &str) {
    let Ok(readme_md_raw) = fs::read_to_string("README.md") else {
        return;
    };
    if !readme_md_raw.contains("https://img.shields.io/badge/version-v") {
        return;
    }
    let mut updated = Vec::new();
    for line in readme_md_raw.lines() {
        if line.contains("https://img.shields.io/badge/version-v") {
            updated.push(format!(
                "![SUSI Version](https://img.shields.io/badge/version-v{}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)",
                version
            ));
        } else {
            updated.push(line.to_string());
        }
    }
    let new_readme = updated.join("\n") + "\n";
    if new_readme != readme_md_raw {
        let _ = write_atomic(Path::new("README.md"), &new_readme);
    }
}

/// Write via temp file + rename: the root and `susi-gawd-agents` build
/// scripts run concurrently and both read these files, so a plain
/// truncate-then-write could hand the other script a half-written ledger.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}
