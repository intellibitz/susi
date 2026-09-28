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

fn main() {
    let mut args: Vec<String> = env::args().collect();
    args.remove(0);

    let is_build = args.first().map(|s| s.as_str()) == Some("build");

    let mut cmd = Command::new("cargo");
    if is_build {
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
    use super::with_detected_features;

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
}
