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

fn detect_gpu_features() -> (Vec<String>, Option<String>) {
    let mut args = Vec::new();
    let mut cudarc_ver = None;

    if cfg!(target_os = "macos") {
        args.push("--features".to_string());
        args.push("metal".to_string());
    } else if cfg!(target_os = "linux") || cfg!(target_os = "windows") {
        // Simple nvcc check for cuda
        let nvcc_check = Command::new("nvcc")
            .arg("--version")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).to_string());
        let mut has_cuda = false;

        if let Ok(out) = nvcc_check {
            if let Some(idx) = out.find("release ") {
                let rest = &out[idx + 8..];
                let ver = rest.split(',').next().unwrap_or("").trim();
                if ver.starts_with("11.") || ver.starts_with("12.") || ver.starts_with("13.") {
                    has_cuda = true;
                    if ver.starts_with("13.") {
                        if let Some(minor) = ver.split('.').nth(1) {
                            if let Ok(m) = minor.parse::<u32>() {
                                if m > 3 {
                                    cudarc_ver = Some("13030".to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        if has_cuda || Path::new("/usr/local/cuda").exists() {
            args.push("--features".to_string());
            args.push("cuda".to_string());
        }
    }

    (args, cudarc_ver)
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
    match Command::new("bash")
        .current_dir(dir)
        .arg("scripts/setup-dev.sh")
        .status()
    {
        Ok(s) if s.success() => println!("xtask: enabled git hooks + ledger merge driver."),
        Ok(_) | Err(_) => eprintln!("xtask: could not run scripts/setup-dev.sh (skipping)."),
    }
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    args.remove(0);

    let is_build = args.first().map(|s| s.as_str()) == Some("build");

    let mut cmd = Command::new("cargo");
    if is_build {
        ensure_dev_setup();
        println!("xtask: cargo build running GPU detection...");
        let (gpu_args, cudarc_ver) = detect_gpu_features();
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

        if let Some(cv) = cudarc_ver {
            cmd.env("CUDARC_CUDA_VERSION", cv);
        }
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
    use super::{ensure_dev_setup_in, with_detected_features};
    use std::process::Command;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn detected_features_append_when_caller_chose_none() {
        assert_eq!(
            with_detected_features(&v(&["build", "--release"]), v(&["--features", "cuda"])),
            v(&["build", "--release", "--features", "cuda"])
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
                with_detected_features(&explicit, v(&["--features", "cuda"])),
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
