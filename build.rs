#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Build scripts are not production runtime modules: a failed invariant must
// abort the build loudly, so panic-on-error is the intended failure mode here.
#![allow(missing_docs)]
//! Root package build script: GPU feature warning + agent-ledger version sync.
use serde_json::Value;
use std::env;
use std::fs;
use std::path::Path;

fn main() {
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
        fs::write(path, pretty + "\n").expect("write synced version");
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
        let _ = fs::write("README.md", new_readme);
    }
}

/// A bare `cargo build`/`cargo build --release` silently produces a
/// CPU-only binary even on a host with a real GPU — `cuda`/`metal` are
/// opt-in Cargo features Cargo has no way to auto-enable from host
/// detection, and this build script's own crate has no mechanism to force
/// them on for the invoking build. What it *can* do is make the omission
/// loud instead of silent: a live benchmark measured a 17x tokens/sec
/// difference between the two on identical hardware (EV-2022920-035,
/// `.agents/evidence.json`) — that gap is worth a warning every time it's
/// about to happen unnoticed.
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
            "cargo:warning=NVIDIA GPU detected but this build does not enable the `cuda` \
             feature — local inference will run CPU-only. A live benchmark on comparable \
             hardware measured a 17x tokens/sec difference (EV-2022920-035, .agents/evidence.json). \
             Use `./build-gpu.sh` instead of `cargo build` directly — it detects the GPU and \
             applies the CUDA-toolkit-version workaround `--features cuda` alone needs on newer \
             CUDA releases."
        );
    }
}
