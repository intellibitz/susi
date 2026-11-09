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

mod build_time;
mod compiler_env;
mod extension_watch;
// Shared with the local-workflow binary, which writes the receipts this
// binary verifies; the writer half is unused here.
#[allow(dead_code)]
mod gate_receipt;
mod local_build;
// Shared with the local-release binary, which also uses `select_product_features`;
// this binary only needs `product_feature_args` (local-build).
#[allow(dead_code)]
mod product_features;
mod sysroot_sources;
// Shared with the launcher; public receipt replay rebuilds fresh evidence.
#[allow(dead_code)]
mod test_binary_evidence;
#[allow(dead_code)]
mod test_runner;
mod trusted_sysroot;

#[cfg(windows)]
extern crate susi_windows_runtime;

use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

/// Writes a tracked repository file in place. xtask only builds and never
/// installs (Mandate 48), so it does no rename-based placement; these files are
/// under version control, which is the recovery path for an interrupted write.
fn write_tracked_file(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::write(path, contents)
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
        write_tracked_file(path, &format!("{pretty}\n"))
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
        write_tracked_file(&readme_path, &new_readme)
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
    let has_nvidia_gpu = susi_paths::console::background_command("nvidia-smi")
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

fn collect_known_extension_ids(val: &Value, top_level: bool, out: &mut Vec<String>) {
    match val {
        Value::Array(items) => {
            for item in items {
                if top_level {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                        continue;
                    }
                }
                collect_known_extension_ids(item, false, out);
            }
        }
        Value::Object(map) => {
            if let Some(ext_id) = map.get("extension_id").and_then(Value::as_str) {
                if !ext_id.is_empty() {
                    out.push(ext_id.to_string());
                }
            }
            if let Some(Value::Array(known)) = map.get("known_extensions") {
                for item in known {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                    }
                }
            }
            for child in map.values() {
                collect_known_extension_ids(child, false, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Maps dedicated `xtask` binary subcommands (`startup-gate`, `local-channel`,
/// `installed-upgrade`) so `cargo xtask <subcommand> [args...]` forwards directly
/// to the corresponding binary instead of falling through to `cargo <subcommand>` (`D-SP-2`, `D-OS-6`).
pub fn xtask_bin_subcommand(sub: &str) -> Option<&'static str> {
    match sub {
        "startup-gate" => Some("startup-gate"),
        "local-channel" => Some("local-channel"),
        "installed-upgrade" => Some("installed-upgrade"),
        _ => None,
    }
}

/// Builds the `cargo run --offline --locked -p xtask --bin <bin_name> -- <tail_args...>` command
/// used to forward dedicated `xtask` binary subcommands (`D-SP-2`, `D-OS-6`).
pub fn build_xtask_bin_forward_command(bin_name: &str, tail_args: &[String]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--offline",
        "--locked",
        "-p",
        "xtask",
        "--bin",
        bin_name,
    ]);
    if !tail_args.is_empty() {
        cmd.arg("--");
        cmd.args(tail_args);
    }
    cmd
}

/// Parses `published_path` and diffs official extensions against `roadmap_path` (when present),
/// returning deterministic [`extension_watch::RoadmapItem`] entries for new official extensions.
pub fn run_extension_watch_on_files(
    published_path: &Path,
    roadmap_path: Option<&Path>,
) -> Result<Vec<extension_watch::RoadmapItem>, String> {
    let raw_published = fs::read_to_string(published_path)
        .map_err(|e| format!("failed to read {}: {e}", published_path.display()))?;
    let published = extension_watch::parse_published(&raw_published)?;
    let _spec_bytes: usize = published.iter().map(|ext| ext.spec_url.len()).sum();

    let mut known_ids = Vec::new();
    if let Some(rpath) = roadmap_path {
        if rpath.is_file() {
            let raw_roadmap = fs::read_to_string(rpath)
                .map_err(|e| format!("failed to read {}: {e}", rpath.display()))?;
            let doc: Value = serde_json::from_str(&raw_roadmap)
                .map_err(|e| format!("invalid JSON {}: {e}", rpath.display()))?;
            collect_known_extension_ids(&doc, true, &mut known_ids);
        }
    }
    Ok(extension_watch::diff_new_official(&published, &known_ids))
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

    if args.first().map(String::as_str) == Some("verify-receipt") {
        if let Err(error) = gate_receipt::run(&args[1..]) {
            eprintln!("xtask: {error}");
            exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some("build-time") {
        if let Err(error) = build_time::run(&args[1..]) {
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

    if args.first().map(String::as_str) == Some("extension-watch") {
        let Some(published_arg) = args.get(1) else {
            eprintln!("xtask: usage: extension-watch <published.json> [roadmap.json]");
            exit(1);
        };
        let roadmap_path = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".agents/roadmap.json"));
        match run_extension_watch_on_files(Path::new(published_arg), Some(&roadmap_path)) {
            Ok(items) => {
                let json_items: Vec<Value> = items
                    .into_iter()
                    .map(|item| {
                        serde_json::json!({
                            "key": item.key,
                            "title": item.title,
                            "extension_id": item.extension_id,
                        })
                    })
                    .collect();
                match serde_json::to_string_pretty(&json_items) {
                    Ok(text) => {
                        println!("{text}");
                        return;
                    }
                    Err(error) => {
                        eprintln!("xtask: {error}");
                        exit(1);
                    }
                }
            }
            Err(error) => {
                eprintln!("xtask: {error}");
                exit(1);
            }
        }
    }

    if let Some(bin_name) = args.first().and_then(|s| xtask_bin_subcommand(s.as_str())) {
        let mut forward = build_xtask_bin_forward_command(bin_name, &args[1..]);
        match forward.status() {
            Ok(s) if s.success() => return,
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("xtask: failed to execute {bin_name}: {e}");
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
        build_xtask_bin_forward_command, detected_features_for_os, ensure_dev_setup_in,
        requests_retired_backend, run_extension_watch_on_files, sync_workspace_version_and_badge,
        with_detected_features, xtask_bin_subcommand,
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

    #[test]
    fn run_extension_watch_on_files_filters_known_and_experimental_items() {
        let dir = std::env::temp_dir().join(format!("xtask-ext-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let published_path = dir.join("published.json");
        let roadmap_path = dir.join("roadmap.json");
        std::fs::write(
            &published_path,
            r#"{
                "extensions": [
                    {"id": "zeta/new-official", "status": "official", "spec_url": "https://example.com/zeta.json"},
                    {"id": "acme/already-known", "status": "official", "spec_url": "https://example.com/known.json"},
                    {"id": "alpha/new-official", "status": "Official", "spec_url": "https://example.com/alpha.json"},
                    {"id": "beta/experimental", "status": "experimental", "spec_url": "https://example.com/beta.json"}
                ]
            }"#,
        )
        .unwrap();
        std::fs::write(
            &roadmap_path,
            r#"{
                "schema": "susi/roadmap/v1",
                "items": [
                    {"key": "ext-watch-acme-already-known", "extension_id": "acme/already-known"}
                ]
            }"#,
        )
        .unwrap();

        let items = run_extension_watch_on_files(&published_path, Some(&roadmap_path)).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "ext-watch-alpha-new-official");
        assert_eq!(items[0].extension_id, "alpha/new-official");
        assert_eq!(items[1].key, "ext-watch-zeta-new-official");
        assert_eq!(items[1].extension_id, "zeta/new-official");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn routes_xtask_binary_subcommands_with_offline_locked_forward_args() {
        assert_eq!(xtask_bin_subcommand("startup-gate"), Some("startup-gate"));
        assert_eq!(xtask_bin_subcommand("local-channel"), Some("local-channel"));
        assert_eq!(
            xtask_bin_subcommand("installed-upgrade"),
            Some("installed-upgrade")
        );
        assert_eq!(xtask_bin_subcommand("build"), None);
        assert_eq!(xtask_bin_subcommand("test"), None);

        let cmd_no_tail = build_xtask_bin_forward_command("startup-gate", &[]);
        let args_no_tail: Vec<String> = cmd_no_tail
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args_no_tail,
            v(&[
                "run",
                "--offline",
                "--locked",
                "-p",
                "xtask",
                "--bin",
                "startup-gate"
            ])
        );

        let cmd_with_tail =
            build_xtask_bin_forward_command("local-channel", &v(&["status", "--check"]));
        let args_with_tail: Vec<String> = cmd_with_tail
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args_with_tail,
            v(&[
                "run",
                "--offline",
                "--locked",
                "-p",
                "xtask",
                "--bin",
                "local-channel",
                "--",
                "status",
                "--check"
            ])
        );
    }
}
