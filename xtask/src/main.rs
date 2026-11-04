#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

// Extension-watch logic lands ahead of its CLI wiring; keep the pure API
// compiled (and unit-tested) without emitting dead-code noise.
mod compiler_env;
#[allow(dead_code)]
mod extension_watch;
mod local_build;
mod sysroot_sources;
mod trusted_sysroot;

#[cfg(windows)]
extern crate susi_windows_runtime;

use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn sync_json_version(path: &Path, expected_schema: &str, version: &str) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let raw =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut doc = serde_json::from_str::<Value>(&raw)
        .map_err(|e| format!("invalid JSON {}: {e}", path.display()))?;
    let schema = doc.get("schema").and_then(Value::as_str).unwrap_or("");
    if schema != expected_schema {
        return Err(format!(
            "{}: expected schema {expected_schema:?}, got {schema:?}",
            path.display()
        ));
    }
    if doc.get("version").and_then(Value::as_str) != Some(version) {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("version".to_string(), Value::String(version.to_string()));
        }
        let pretty = serde_json::to_string_pretty(&doc)
            .map_err(|e| format!("failed to serialize {}: {e}", path.display()))?;
        write_atomic(path, &format!("{pretty}\n"))
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn sync_readme_badge_in(root: &Path, version: &str) -> Result<(), String> {
    let readme_path = root.join("README.md");
    if !readme_path.is_file() {
        return Ok(());
    }
    let readme_md_raw = fs::read_to_string(&readme_path)
        .map_err(|e| format!("failed to read {}: {e}", readme_path.display()))?;
    if !readme_md_raw.contains("https://img.shields.io/badge/version-v") {
        return Ok(());
    }
    let mut updated = Vec::new();
    for line in readme_md_raw.lines() {
        if line.contains("https://img.shields.io/badge/version-v") {
            updated.push(format!(
                "![SUSI Version](https://img.shields.io/badge/version-v{version}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)"
            ));
        } else {
            updated.push(line.to_string());
        }
    }
    let new_readme = format!("{}\n", updated.join("\n"));
    if new_readme != readme_md_raw {
        write_atomic(&readme_path, &new_readme)
            .map_err(|e| format!("failed to write {}: {e}", readme_path.display()))?;
    }
    Ok(())
}

/// Synchronizes the workspace version from `<root>/Cargo.toml` across
/// `.agents/{identity,roadmap,evidence}.json` and the `README.md` version badge (`D-SP-1`).
pub fn sync_workspace_version_and_badge(root: &Path) -> Result<String, String> {
    let cargo_toml_path = root.join("Cargo.toml");
    let cargo_toml = fs::read_to_string(&cargo_toml_path)
        .map_err(|e| format!("missing {}: {e}", cargo_toml_path.display()))?;
    let version = cargo_toml
        .lines()
        .find(|l| l.trim().starts_with("version = \""))
        .and_then(|l| l.split('"').nth(1))
        .ok_or_else(|| format!("could not find version in {}", cargo_toml_path.display()))?
        .to_string();

    for (rel, schema) in [
        (".agents/identity.json", "susi/identity/v1"),
        (".agents/roadmap.json", "susi/roadmap/v1"),
        (".agents/evidence.json", "susi/evidence/v1"),
    ] {
        sync_json_version(&root.join(rel), schema, &version)?;
    }

    sync_readme_badge_in(root, &version)?;
    Ok(version)
}

/// Reports the selected CPU backend when an NVIDIA device is present (`D-SP-1`).
pub fn warn_if_gpu_available_but_unused(os: &str, features_include_metal: bool) {
    if features_include_metal || os == "macos" {
        return;
    }
    let has_nvidia_gpu = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
    if has_nvidia_gpu {
        println!(
            "xtask: [build-gpu] NVIDIA GPU detected; this build uses CPU inference. \
             The supported native local CPU workflow is `cargo local-build debug` or \
             `cargo local-build production`. CUDA/MKL product features are retired because \
             their dependencies fail license/native-code gates."
        );
    }
}

/// Appends detected GPU feature args unless the caller already chose a
/// feature set (`--features`/`-F`, `--all-features`, `--no-default-features`):
/// an explicit choice (e.g. `--features metal`) must not silently gain a
/// second backend.
fn with_detected_features(args: &[String], detected: Vec<String>) -> Vec<String> {
    let explicit = args.iter().any(|a| {
        a == "--features"
            || a == "-F"
            || a.starts_with("--features=")
            || (a.starts_with("-F") && a.len() > 2)
            || a == "--all-features"
            || a == "--no-default-features"
    });
    if explicit && !detected.is_empty() {
        println!("xtask: explicit feature flags given; not injecting detected GPU features.");
    }
    let mut out = args.to_vec();
    if !explicit {
        out.extend(detected);
    }
    out
}

fn requests_retired_backend(features: &str) -> bool {
    features
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .any(|feature| matches!(feature.rsplit('/').next(), Some("cuda" | "mkl")))
}

/// Only Metal is a supported optional in-process accelerator. NVIDIA hardware
/// discovery remains a runtime inventory fact; it cannot opt builds into CUDA.
fn detected_features_for_os(os: &str) -> Vec<String> {
    if os == "macos" {
        vec!["--features".to_string(), "metal".to_string()]
    } else {
        Vec::new()
    }
}

/// Best-effort: enable the repo's git hooks and ledger merge driver once per
/// clone (config is shared by every worktree) so parallel agents and users
/// get the same pre-commit/pre-push gate without remembering to opt in.
fn ensure_dev_setup() {
    ensure_dev_setup_in(Path::new("."));
}

fn ensure_dev_setup_in(dir: &Path) {
    let configured = Command::new("git")
        .current_dir(dir)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == ".githooks")
        .unwrap_or(false);
    if configured || !dir.join("scripts/setup-dev.sh").is_file() {
        return;
    }
    // On Windows ash may resolve to the WSL launcher (whose /bin/bash is absent);
    // the repository's scripts are POSIX and run with Git's sh everywhere else.
    let shell = if cfg!(windows) { "sh" } else { "bash" };
    match Command::new(shell)
        .current_dir(dir)
        .arg("scripts/setup-dev.sh")
        .status()
    {
        Ok(s) if s.success() => println!("xtask: enabled git hooks + ledger merge driver."),
        Ok(_) | Err(_) => eprintln!("xtask: could not run scripts/setup-dev.sh (skipping)."),
    }
}

fn main() {
    if env::var("SUSI_BUILD_FEATURES").is_ok_and(|features| requests_retired_backend(&features)) {
        eprintln!("SUSI_BUILD_FEATURES requests retired CUDA/MKL support; these dependencies fail product license/native-code gates.");
        exit(1);
    }
    let mut args: Vec<String> = env::args().collect();
    args.remove(0);

    if args.first().map(String::as_str) == Some("local-build") {
        if let Err(error) = local_build::run(&args[1..]) {
            eprintln!("xtask: {error}");
            exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some("sync-version") {
        let root = args
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        match sync_workspace_version_and_badge(&root) {
            Ok(ver) => {
                println!("xtask: synced workspace version v{ver}.");
                return;
            }
            Err(error) => {
                eprintln!("xtask: {error}");
                exit(1);
            }
        }
    }

    let is_build = args.first().map(|s| s.as_str()) == Some("build");

    let mut cmd = Command::new("cargo");
    if is_build {
        ensure_dev_setup();
        let _ = sync_workspace_version_and_badge(Path::new("."));
        println!("xtask: cargo build running GPU detection...");
        let gpu_args = detected_features_for_os(env::consts::OS);
        let has_metal =
            gpu_args.iter().any(|a| a == "metal") || args.iter().any(|a| a.contains("metal"));
        warn_if_gpu_available_but_unused(env::consts::OS, has_metal);
        if gpu_args.is_empty() {
            println!(
                "xtask: [build-gpu] No GPU backend detected (or unsupported) - building CPU-only."
            );
        } else {
            println!(
                "xtask: [build-gpu] GPU backend detected: building with {}",
                gpu_args.join(" ")
            );
        }

        cmd.args(with_detected_features(&args, gpu_args));
    } else {
        cmd.args(&args);
    }

    let status = cmd.status();

    match status {
        Ok(s) if !s.success() => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("Failed to execute cargo: {}", e);
            exit(1);
        }
        _ => {}
    }

    if is_build {
        // Dev builds never install: ~/.susi/bin/susi (and the daemon that
        // runs it) belongs to tagged releases only, so the local susi stays
        // a stable toolchain for building the next susi. Releases are
        // promoted by scripts/susi-release-sync.sh.
        let profile_dir = if args.iter().any(|arg| arg == "--release") {
            "release"
        } else {
            "debug"
        };
        let target_dir =
            env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
        println!(
            "xtask: built {} (not installed; the local susi is updated only by releases via scripts/susi-release-sync.sh).",
            target_dir.join(profile_dir).join("susi").display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detected_features_for_os, ensure_dev_setup_in, requests_retired_backend,
        sync_workspace_version_and_badge, with_detected_features,
    };
    use serde_json::Value;
    use std::process::Command;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn retired_feature_overrides_cannot_be_silently_ignored() {
        for features in [
            "cuda",
            "mkl",
            "metal,cuda",
            "metal mkl",
            "susi-gemi/cuda",
            "susi-vendor-candle/mkl",
        ] {
            assert!(requests_retired_backend(features), "{features}");
        }
        for features in ["", "metal", "tokio-console"] {
            assert!(!requests_retired_backend(features), "{features}");
        }
    }

    #[test]
    fn hardware_detection_never_selects_retired_backends() {
        for os in ["linux", "windows", "freebsd"] {
            assert!(detected_features_for_os(os).is_empty());
        }
        assert_eq!(
            detected_features_for_os("macos"),
            v(&["--features", "metal"])
        );
    }

    #[test]
    fn detected_features_append_when_caller_chose_none() {
        assert_eq!(
            with_detected_features(&v(&["build", "--release"]), v(&["--features", "metal"])),
            v(&["build", "--release", "--features", "metal"])
        );
    }

    #[test]
    fn explicit_feature_choices_are_never_augmented() {
        for explicit in [
            v(&["build", "--features", "metal"]),
            v(&["build", "--features=metal"]),
            v(&["build", "-F", "metal"]),
            v(&["build", "-Fmetal"]),
            v(&["build", "--no-default-features"]),
        ] {
            assert_eq!(
                with_detected_features(&explicit, v(&["--features", "metal"])),
                explicit
            );
        }
    }

    fn scratch_repo(tag: &str, setup_body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        assert!(Command::new("git")
            .current_dir(&dir)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.join("scripts/setup-dev.sh"), setup_body).unwrap();
        dir
    }

    fn hooks_path(dir: &std::path::Path) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["config", "--get", "core.hooksPath"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn dev_setup_runs_the_script_once_when_hooks_are_off() {
        let dir = scratch_repo("on", "git config core.hooksPath .githooks\n");
        assert_eq!(hooks_path(&dir), "");
        ensure_dev_setup_in(&dir);
        assert_eq!(hooks_path(&dir), ".githooks");
        ensure_dev_setup_in(&dir); // already configured: no-op
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_setup_tolerates_a_failing_or_missing_script() {
        let dir = scratch_repo("fail", "exit 3\n");
        ensure_dev_setup_in(&dir);
        assert_eq!(hooks_path(&dir), "");
        std::fs::remove_file(dir.join("scripts/setup-dev.sh")).unwrap();
        ensure_dev_setup_in(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_workspace_version_updates_ledgers_and_readme_badge_idempotently() {
        let dir = std::env::temp_dir().join(format!("xtask-sync-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".agents")).unwrap();

        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"susi\"\nversion = \"0.21.99\"\n",
        )
        .unwrap();
        for (file, schema) in [
            ("identity.json", "susi/identity/v1"),
            ("roadmap.json", "susi/roadmap/v1"),
            ("evidence.json", "susi/evidence/v1"),
        ] {
            std::fs::write(
                dir.join(".agents").join(file),
                format!("{{\"schema\":\"{schema}\",\"version\":\"0.21.1\"}}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("README.md"),
            "# SUSI\n![SUSI Version](https://img.shields.io/badge/version-v0.21.1-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)\n",
        )
        .unwrap();

        let ver = sync_workspace_version_and_badge(&dir).unwrap();
        assert_eq!(ver, "0.21.99");

        for file in ["identity.json", "roadmap.json", "evidence.json"] {
            let raw = std::fs::read_to_string(dir.join(".agents").join(file)).unwrap();
            let doc: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(doc.get("version").and_then(Value::as_str), Some("0.21.99"));
        }
        let readme_after = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(readme_after.contains("https://img.shields.io/badge/version-v0.21.99-blue.svg"));

        // Second call is idempotent.
        let ver_again = sync_workspace_version_and_badge(&dir).unwrap();
        assert_eq!(ver_again, "0.21.99");
        assert_eq!(
            std::fs::read_to_string(dir.join("README.md")).unwrap(),
            readme_after
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_workspace_version_rejects_mismatched_ledger_schema() {
        let dir = std::env::temp_dir().join(format!("xtask-sync-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".agents")).unwrap();

        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"susi\"\nversion = \"0.21.99\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".agents/identity.json"),
            "{\"schema\":\"wrong/schema/v1\",\"version\":\"0.21.1\"}\n",
        )
        .unwrap();

        let err = sync_workspace_version_and_badge(&dir).unwrap_err();
        assert!(
            err.contains("expected schema \"susi/identity/v1\""),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
