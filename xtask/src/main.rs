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

use std::env;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

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

    let is_build = args.first().map(|s| s.as_str()) == Some("build");

    let mut cmd = Command::new("cargo");
    if is_build {
        ensure_dev_setup();
        println!("xtask: cargo build running GPU detection...");
        let gpu_args = detected_features_for_os(env::consts::OS);
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
        with_detected_features,
    };
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
}
