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
mod fast_release;
// Shared with the local-workflow binary, which writes the receipts this
// binary verifies; the writer half is unused here.
#[allow(dead_code)]
mod gate_receipt;
mod local_build;
// Receipt verification replays test binaries through these local-workflow
// modules; their writer and gate-launch halves are unused by this binary.
#[allow(dead_code)]
mod native_provenance;
// Shared with the local-release binary, which also uses `select_product_features`;
// this binary only needs `product_feature_args` (local-build).
#[allow(dead_code)]
mod product_features;
mod sysroot_sources;
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
use std::process::{exit, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

// Keep these values identical to scripts/setup-dev.sh. Windows configures Git
// directly, without requiring a shell just to enable development setup. The
// legacy merge drivers themselves still require their existing shell/jq tools.
// Write hooks last: a failed driver configuration must not mark setup complete.
const DEV_SETUP_CONFIG: [(&str, &str); 5] = [
    (
        "merge.ledger.name",
        "union of appended .agents/evidence.json entries",
    ),
    ("merge.ledger.driver", "sh scripts/merge-ledger.sh %O %A %B"),
    (
        "merge.roadmap.name",
        "per-vector merge of .agents/roadmap.json",
    ),
    (
        "merge.roadmap.driver",
        "sh scripts/merge-roadmap.sh %O %A %B",
    ),
    ("core.hooksPath", ".githooks"),
];

/// Best-effort: enable the repo's git hooks and both merge drivers once per
/// clone (config is shared by every worktree) so parallel agents and users
/// get the same pre-commit/pre-push gate without remembering to opt in.
fn ensure_dev_setup() {
    ensure_dev_setup_in(Path::new("."));
}

fn ensure_dev_setup_in(dir: &Path) {
    match dev_setup_in(dir) {
        Ok(true) => println!("xtask: enabled git hooks + ledger/roadmap merge drivers."),
        Ok(false) => {}
        Err(error) => {
            eprintln!("xtask: could not configure development setup ({error}; skipping).")
        }
    }
}

fn dev_setup_in(dir: &Path) -> Result<bool, String> {
    if !dir.join("scripts/setup-dev.sh").is_file() {
        return Ok(false);
    }
    let configured = DEV_SETUP_CONFIG.iter().all(|(key, value)| {
        Command::new("git")
            .current_dir(dir)
            .args(["config", "--local", "--get", key])
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == *value
            })
    });
    if configured {
        return Ok(false);
    }
    if cfg!(windows) {
        for (key, value) in DEV_SETUP_CONFIG {
            let status = Command::new("git")
                .current_dir(dir)
                .args(["config", "--local", key, value])
                .status()
                .map_err(|error| format!("git config {key}: {error}"))?;
            if !status.success() {
                return Err(format!("git config {key}: {status}"));
            }
        }
    } else {
        // Preserve the existing POSIX setup script and its failure behavior.
        let status = Command::new("bash")
            .current_dir(dir)
            .arg("scripts/setup-dev.sh")
            .status()
            .map_err(|error| format!("scripts/setup-dev.sh: {error}"))?;
        if !status.success() {
            return Err(format!("scripts/setup-dev.sh: {status}"));
        }
    }
    Ok(true)
}

#[derive(Debug, Eq, PartialEq)]
struct WorkspaceTestPlan {
    serial_args: Vec<String>,
    passthrough_only: bool,
    build_args: Vec<String>,
    doc_args: Option<Vec<String>>,
    binary_args: Vec<String>,
    jobs: Option<usize>,
    triple: Option<String>,
    target_dir: Option<PathBuf>,
    explicit_package: bool,
}

fn plan_workspace_test(extra_args: &[String]) -> WorkspaceTestPlan {
    let sep = extra_args.iter().position(|a| a == "--");
    let (cargo_tail, binary_args) = match sep {
        Some(idx) => (&extra_args[..idx], extra_args[idx + 1..].to_vec()),
        None => (extra_args, Vec::new()),
    };

    let passthrough_only = cargo_tail
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "--no-run" | "--doc"));
    let explicit_package = cargo_tail.iter().any(|a| {
        a == "-p"
            || a == "--package"
            || a.starts_with("--package=")
            || (a.starts_with("-p") && !a.starts_with("--") && a.len() > 2)
    });
    let has_workspace = cargo_tail
        .iter()
        .any(|a| a == "--workspace" || a == "--all");
    let has_offline = cargo_tail.iter().any(|a| a == "--offline");
    let has_lock_flag = cargo_tail
        .iter()
        .any(|a| matches!(a.as_str(), "--locked" | "--unlocked" | "--frozen"));

    let mut jobs = None;
    let mut triple = None;
    let mut target_dir = None;
    let mut i = 0;
    while i < cargo_tail.len() {
        let arg = cargo_tail[i].as_str();
        if (arg == "-j" || arg == "--jobs") && i + 1 < cargo_tail.len() {
            jobs = cargo_tail[i + 1].parse::<usize>().ok().filter(|&n| n > 0);
            i += 2;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--jobs=") {
            jobs = val.parse::<usize>().ok().filter(|&n| n > 0);
        } else if let Some(val) = arg
            .strip_prefix("-j")
            .filter(|_| !arg.starts_with("--") && arg.len() > 2)
        {
            jobs = val.parse::<usize>().ok().filter(|&n| n > 0);
        } else if arg == "--target" && i + 1 < cargo_tail.len() {
            triple = Some(cargo_tail[i + 1].clone());
            i += 2;
            continue;
        } else if let Some(val) = arg.strip_prefix("--target=") {
            triple = Some(val.to_owned());
        } else if arg == "--target-dir" && i + 1 < cargo_tail.len() {
            target_dir = Some(PathBuf::from(&cargo_tail[i + 1]));
            i += 2;
            continue;
        } else if let Some(val) = arg.strip_prefix("--target-dir=") {
            target_dir = Some(PathBuf::from(val));
        }
        i += 1;
    }

    let mut base = vec!["test".to_owned()];
    if !explicit_package && !has_workspace {
        base.push("--workspace".to_owned());
    }
    if !has_offline {
        base.push("--offline".to_owned());
    }
    if !has_lock_flag {
        base.push("--locked".to_owned());
    }

    let mut serial_args = base.clone();
    serial_args.extend(cargo_tail.iter().cloned());
    if sep.is_some() {
        serial_args.push("--".to_owned());
        serial_args.extend(binary_args.iter().cloned());
    }

    let mut build_args = base.clone();
    build_args.extend(
        cargo_tail
            .iter()
            .filter(|a| a.as_str() != "--no-fail-fast")
            .cloned(),
    );
    build_args.push("--no-run".to_owned());
    if !cargo_tail
        .iter()
        .any(|a| a == "--message-format" || a.starts_with("--message-format="))
    {
        build_args.push("--message-format=json".to_owned());
    }

    let restricts_targets = cargo_tail.iter().any(|a| {
        matches!(
            a.as_str(),
            "--lib"
                | "--bins"
                | "--bin"
                | "--tests"
                | "--test"
                | "--benches"
                | "--bench"
                | "--examples"
                | "--example"
                | "--all-targets"
        ) || a.starts_with("--bin=")
            || a.starts_with("--test=")
            || a.starts_with("--bench=")
            || a.starts_with("--example=")
    });
    let doc_args = if passthrough_only || restricts_targets {
        None
    } else {
        let mut doc = base;
        doc.push("--doc".to_owned());
        doc.extend(cargo_tail.iter().cloned());
        if !doc.iter().any(|a| a == "--no-fail-fast") {
            doc.push("--no-fail-fast".to_owned());
        }
        if !binary_args.is_empty() {
            doc.push("--".to_owned());
            doc.extend(binary_args.iter().cloned());
        }
        Some(doc)
    };

    WorkspaceTestPlan {
        serial_args,
        passthrough_only,
        build_args,
        doc_args,
        binary_args,
        jobs,
        triple,
        target_dir,
        explicit_package,
    }
}

fn json_has_lib_target(stdout: &str) -> bool {
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        if msg
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(Value::as_array)
            .is_some_and(|kinds| {
                kinds.iter().filter_map(Value::as_str).any(|k| {
                    matches!(
                        k,
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
                    )
                })
            })
        {
            return true;
        }
    }
    false
}

fn count_test_results(log: &str) -> (u64, u64, u64, u64) {
    let (mut suites, mut passed, mut failed, mut ignored) = (0u64, 0u64, 0u64, 0u64);
    for line in log.lines() {
        let Some(rest) = line.strip_prefix("test result: ") else {
            continue;
        };
        suites += 1;
        for part in rest.split(';') {
            let mut fields = part.split_whitespace().rev();
            let (Some(label), Some(number)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Ok(number) = number.parse::<u64>() else {
                continue;
            };
            match label {
                "passed" => passed += number,
                "failed" => failed += number,
                "ignored" => ignored += number,
                _ => {}
            }
        }
    }
    (suites, passed, failed, ignored)
}

fn run_workspace_test(root: &Path, extra_args: &[String]) -> Result<(), String> {
    let plan = plan_workspace_test(extra_args);
    if plan.passthrough_only || test_runner::serial_requested() {
        let status = Command::new("cargo")
            .current_dir(root)
            .args(&plan.serial_args)
            .status()
            .map_err(|e| format!("cannot run cargo test: {e}"))?;
        if !status.success() {
            return Err(format!(
                "cargo test failed (exit {})",
                status.code().unwrap_or(1)
            ));
        }
        return Ok(());
    }

    let total_started = Instant::now();
    let build_started = Instant::now();
    let build_out = Command::new("cargo")
        .current_dir(root)
        .args(&plan.build_args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run cargo test --no-run: {e}"))?;
    let build_elapsed = build_started.elapsed();
    let stdout_text = String::from_utf8_lossy(&build_out.stdout);
    let stderr_text = String::from_utf8_lossy(&build_out.stderr);
    if !stderr_text.is_empty() {
        eprint!("{stderr_text}");
    }
    let (binaries, diagnostics) =
        test_runner::parse_artifacts(&stdout_text).map_err(|e| e.to_string())?;
    for diag in &diagnostics {
        eprint!("{diag}");
        if !diag.ends_with('\n') {
            eprintln!();
        }
    }
    if !build_out.status.success() {
        return Err(format!(
            "cargo test --no-run failed (exit {})",
            build_out.status.code().unwrap_or(1)
        ));
    }

    let original_home =
        env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".cargo")));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".rustup")));
    let target_dir = plan
        .target_dir
        .clone()
        .or_else(|| env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
        .map(|p| if p.is_absolute() { p } else { root.join(p) })
        .unwrap_or_else(|| root.join("target"));

    let avail = std::thread::available_parallelism().map_or(4, usize::from);
    let jobs_n = plan.jobs.unwrap_or(avail).max(1);
    let concurrency = jobs_n.clamp(2, 12);
    let total_test_threads = (concurrency * 2).max(jobs_n);
    let build_jobs = (jobs_n / concurrency).max(1);

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = env::temp_dir().join(format!("susi-xtask-test-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let home_root = scratch.join("homes");
    let log_path = scratch.join("tests.log");
    let durations = target_dir.join("xtask-test-durations.json");
    let heavy = test_runner::heavy_binaries(root);
    let settings = test_runner::Settings {
        concurrency,
        total_test_threads,
        build_jobs,
        home_root: &home_root,
        target_dir: &target_dir,
        triple: plan.triple.as_deref(),
        durations: &durations,
        heavy: &heavy,
        log: &log_path,
    };

    let run_started = Instant::now();
    let outcome = test_runner::run(&binaries, &settings, &|binary, private_home| {
        let mut child = Command::new(&binary.executable);
        child.args(&plan.binary_args);
        susi_paths::test_env::isolate_home(&mut child, private_home);
        if let Some(ref ch) = cargo_home {
            child.env("CARGO_HOME", ch);
        }
        if let Some(ref rh) = rustup_home {
            child.env("RUSTUP_HOME", rh);
        }
        let child_target = private_home.join("target");
        for key in [
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
        ] {
            child.env(key, &child_target);
        }
        Ok(child)
    })
    .map_err(|e| {
        let _ = fs::remove_dir_all(&scratch);
        e.to_string()
    })?;
    let run_elapsed = run_started.elapsed();

    let should_run_docs =
        plan.doc_args.is_some() && (!plan.explicit_package || json_has_lib_target(&stdout_text));
    let mut doc_elapsed = Duration::ZERO;
    let mut doc_ok = true;
    let mut ran_docs = false;
    if should_run_docs {
        if let Some(ref doc_args) = plan.doc_args {
            ran_docs = true;
            let doc_home = scratch.join("doc-home");
            fs::create_dir_all(&doc_home).map_err(|e| e.to_string())?;
            let log_append = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|e| format!("{}: {e}", log_path.display()))?;
            let err_append = log_append.try_clone().map_err(|e| e.to_string())?;
            let mut doc_cmd = Command::new("cargo");
            doc_cmd
                .current_dir(root)
                .args(doc_args)
                .stdin(Stdio::null())
                .stdout(log_append)
                .stderr(err_append);
            susi_paths::test_env::isolate_home(&mut doc_cmd, &doc_home);
            if let Some(ref ch) = cargo_home {
                doc_cmd.env("CARGO_HOME", ch);
            }
            if let Some(ref rh) = rustup_home {
                doc_cmd.env("RUSTUP_HOME", rh);
            }
            let doc_started = Instant::now();
            let status = doc_cmd
                .status()
                .map_err(|e| format!("cannot run cargo test --doc: {e}"))?;
            doc_elapsed = doc_started.elapsed();
            doc_ok = status.success();
        }
    }

    let log_text = fs::read_to_string(&log_path).unwrap_or_default();
    let (_suites, passed, failed, ignored) = count_test_results(&log_text);
    let binaries_ok = outcome.failed.is_empty() && outcome.ran == binaries.len();
    let _ = fs::remove_dir_all(&scratch);

    println!(
        "xtask test: {passed} passed; {failed} failed; {ignored} ignored across {} binaries{} in {:.2}s (build={:.2}s, run={:.2}s, doc={:.2}s, lanes={concurrency})",
        binaries.len(),
        if ran_docs { " + doctests" } else { "" },
        total_started.elapsed().as_secs_f64(),
        build_elapsed.as_secs_f64(),
        run_elapsed.as_secs_f64(),
        doc_elapsed.as_secs_f64(),
    );

    if !binaries_ok || !doc_ok {
        eprint!("{log_text}");
        let failed_ids: Vec<&str> = outcome.failed.iter().map(|(id, _)| id.as_str()).collect();
        return Err(format!(
            "workspace tests failed (failed binaries: [{}], doctests_ok={doc_ok})",
            failed_ids.join(", ")
        ));
    }
    Ok(())
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

    if args.first().map(String::as_str) == Some("fast-release") {
        if let Err(error) = fast_release::run(&args[1..]) {
            eprintln!("xtask: {error}");
            exit(1);
        }
        return;
    }

    if matches!(
        args.first().map(String::as_str),
        Some("test" | "workspace-test")
    ) {
        if let Err(error) = run_workspace_test(Path::new("."), &args[1..]) {
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
        build_xtask_bin_forward_command, detected_features_for_os, dev_setup_in,
        ensure_dev_setup_in, requests_retired_backend, run_extension_watch_on_files,
        sync_workspace_version_and_badge, with_detected_features, xtask_bin_subcommand,
        DEV_SETUP_CONFIG,
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
        for features in ["", "metal"] {
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
        git_config(dir, "core.hooksPath")
    }

    fn git_config(dir: &std::path::Path, key: &str) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["config", "--local", "--get", key])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn dev_setup_configures_hooks_and_both_drivers_once() {
        let dir = scratch_repo("on", include_str!("../../scripts/setup-dev.sh"));
        assert_eq!(hooks_path(&dir), "");
        ensure_dev_setup_in(&dir);
        assert_eq!(hooks_path(&dir), ".githooks");
        for (key, value) in DEV_SETUP_CONFIG {
            assert_eq!(git_config(&dir, key), value, "{key}");
        }
        let before = std::fs::read(dir.join(".git/config")).unwrap();
        std::fs::write(dir.join(".git/config.lock"), "held by regression\n").unwrap();
        ensure_dev_setup_in(&dir); // already configured: no-op
        assert!(!dev_setup_in(&dir).unwrap()); // must not try a locked write
        assert_eq!(std::fs::read(dir.join(".git/config")).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_setup_tolerates_a_failing_or_missing_script() {
        let dir = scratch_repo("fail", "exit 3\n");
        // Windows configures Git directly; fail that real operation rather than
        // requiring an unavailable shell to execute the POSIX failure fixture.
        #[cfg(windows)]
        std::fs::write(dir.join(".git/config.lock"), "held by regression\n").unwrap();
        ensure_dev_setup_in(&dir);
        assert_eq!(hooks_path(&dir), "");
        assert!(dev_setup_in(&dir).is_err());
        #[cfg(windows)]
        std::fs::remove_file(dir.join(".git/config.lock")).unwrap();
        std::fs::remove_file(dir.join("scripts/setup-dev.sh")).unwrap();
        ensure_dev_setup_in(&dir);
        assert_eq!(hooks_path(&dir), "");
        assert!(!dev_setup_in(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_setup_repairs_partial_configuration() {
        let dir = scratch_repo("partial", include_str!("../../scripts/setup-dev.sh"));
        assert!(Command::new("git")
            .current_dir(&dir)
            .args(["config", "--local", "core.hooksPath", ".githooks"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(&dir)
            .args(["config", "--local", "merge.ledger.driver", "stale-driver"])
            .status()
            .unwrap()
            .success());
        assert!(dev_setup_in(&dir).unwrap());
        for (key, value) in DEV_SETUP_CONFIG {
            assert_eq!(git_config(&dir, key), value, "{key}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_setup_retries_after_git_configuration_is_unlocked() {
        let dir = scratch_repo("retry", include_str!("../../scripts/setup-dev.sh"));
        std::fs::write(dir.join(".git/config.lock"), "held by regression\n").unwrap();
        assert!(dev_setup_in(&dir).is_err());
        for (key, _) in DEV_SETUP_CONFIG {
            assert_eq!(git_config(&dir, key), "", "{key}");
        }
        std::fs::remove_file(dir.join(".git/config.lock")).unwrap();
        assert!(dev_setup_in(&dir).unwrap());
        for (key, value) in DEV_SETUP_CONFIG {
            assert_eq!(git_config(&dir, key), value, "{key}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_setup_native_values_match_the_legacy_script() {
        let script = include_str!("../../scripts/setup-dev.sh");
        for (key, value) in DEV_SETUP_CONFIG {
            let quoted = format!("git config {key} \"{value}\"");
            let plain = format!("git config {key} {value}");
            assert!(
                script.lines().any(|line| line == quoted || line == plain),
                "native value for {key} drifted from scripts/setup-dev.sh"
            );
        }
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

    #[test]
    fn workspace_test_plan_defaults_and_splits_filter_arguments() {
        let default_plan = super::plan_workspace_test(&[]);
        assert!(!default_plan.passthrough_only);
        assert!(!default_plan.explicit_package);
        assert_eq!(
            default_plan.build_args,
            v(&[
                "test",
                "--workspace",
                "--offline",
                "--locked",
                "--no-run",
                "--message-format=json"
            ])
        );
        assert_eq!(
            default_plan.doc_args,
            Some(v(&[
                "test",
                "--workspace",
                "--offline",
                "--locked",
                "--doc",
                "--no-fail-fast"
            ]))
        );

        let pkg_plan = super::plan_workspace_test(&v(&[
            "-p",
            "susi-core",
            "-j",
            "8",
            "--no-fail-fast",
            "--",
            "acp",
            "--nocapture",
        ]));
        assert!(pkg_plan.explicit_package);
        assert_eq!(pkg_plan.jobs, Some(8));
        assert_eq!(pkg_plan.binary_args, v(&["acp", "--nocapture"]));
        assert_eq!(
            pkg_plan.build_args,
            v(&[
                "test",
                "--offline",
                "--locked",
                "-p",
                "susi-core",
                "-j",
                "8",
                "--no-run",
                "--message-format=json"
            ])
        );
        assert_eq!(
            pkg_plan.doc_args,
            Some(v(&[
                "test",
                "--offline",
                "--locked",
                "--doc",
                "-p",
                "susi-core",
                "-j",
                "8",
                "--no-fail-fast",
                "--",
                "acp",
                "--nocapture"
            ]))
        );

        let bin_only = super::plan_workspace_test(&v(&["-p", "xtask", "--bin", "local-workflow"]));
        assert_eq!(bin_only.doc_args, None);
    }

    #[test]
    fn workspace_test_helpers_detect_lib_artifacts_and_sum_results() {
        let no_lib =
            "{\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"bin\"]}}\n{\"reason\":\"build-finished\",\"success\":true}\n";
        assert!(!super::json_has_lib_target(no_lib));
        let with_lib =
            "{\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"rlib\"]}}\n{\"reason\":\"build-finished\",\"success\":true}\n";
        assert!(super::json_has_lib_target(with_lib));

        let log = "test result: ok. 12 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out\n\
                   test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert_eq!(super::count_test_results(log), (2, 16, 0, 2));
    }
}
