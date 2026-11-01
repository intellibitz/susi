#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Build scripts are not production runtime modules: a failed invariant must
// abort the build loudly, so panic-on-error is the intended failure mode here.
#![allow(missing_docs)]
//! Root package build script: GPU feature warning + agent-ledger version sync.
use serde_json::Value;
use std::env;
use std::fs;
use std::path::Path;

#[path = "xtask/bootstrap/os_ucrt.rs"]
mod os_ucrt;

fn main() {
    os_ucrt::configure().expect("Windows OS-UCRT boundary configuration failed");
    warn_if_gpu_available_but_unused();

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
        (".agents/evidence.json", "susi/evidence/v1"),
    ] {
        sync_json_version(Path::new(rel), schema, &version);
        println!("cargo:rerun-if-changed={rel}");
    }

    sync_readme_badge(&version);
    println!("cargo:rerun-if-changed=README.md");
    println!("cargo:rerun-if-changed=Cargo.toml");
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

/// Report the selected CPU backend when an NVIDIA device is present.
/// GPU acceleration is optional and requires separate platform, toolchain,
/// license and runtime qualification; device detection is not that evidence.
fn warn_if_gpu_available_but_unused() {
    if env::var_os("CARGO_FEATURE_CUDA").is_some() || env::var_os("CARGO_FEATURE_METAL").is_some() {
        return;
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        return;
    }

    let has_nvidia_gpu = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);

    if has_nvidia_gpu {
        println!(
            "cargo:warning=NVIDIA GPU detected; this build uses CPU inference. \
             The supported native local CPU workflow is `cargo local-build debug` or \
             `cargo local-build production`. Optional CUDA acceleration requires separate \
             platform/toolchain, dependency-license and runtime qualification; detecting \
             a GPU does not establish support or a performance improvement for this build."
        );
    }
}
