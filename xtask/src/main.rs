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
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

mod architecture;

fn get_home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

    if args.first().map(String::as_str) == Some("verify-architecture") {
        if let Err(error) = architecture::verify() {
            eprintln!("architecture verification failed:\n{error}");
            exit(1);
        }
        println!("architecture verified: Cargo edges are isolated and shared sources are in sync");
        return;
    }

    let is_build = args.first().map(|s| s.as_str()) == Some("build");

    let mut cmd = Command::new("cargo");
    // Build args (incl. injected GPU features) for the install marker.
    let mut final_features: Vec<String> = Vec::new();

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

        let final_args = with_detected_features(&args, gpu_args);
        cmd.args(&final_args);
        final_features = final_args;

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
        println!("xtask: cargo build finished. Applying post-build hooks...");

        let target_dir = Path::new("target");
        let release = args.iter().any(|arg| arg == "--release");
        let profile_dir = if release { "release" } else { "debug" };

        let bin_name = if cfg!(target_os = "windows") {
            "susi.exe"
        } else {
            "susi"
        };
        let built_bin = target_dir.join(profile_dir).join(bin_name);

        if !built_bin.exists() {
            eprintln!(
                "xtask: Expected binary at {} not found.",
                built_bin.display()
            );
            exit(1);
        }

        let home = get_home_dir();
        let bin_dir = home.join(".susi").join("bin");
        fs::create_dir_all(&bin_dir).unwrap_or_else(|e| {
            eprintln!("xtask: Failed to create ~/.susi/bin: {}", e);
            exit(1);
        });

        let dest = bin_dir.join(bin_name);
        println!(
            "xtask: Installing {} to {}",
            built_bin.display(),
            dest.display()
        );

        // Copy to a sibling temp file, then rename over the destination.
        // Writing in place fails with ETXTBSY when the installed daemon is
        // currently running; rename swaps the path atomically and the running
        // process keeps its old inode.
        let tmp_dest = bin_dir.join(format!("{}.new", bin_name));
        let copy_result =
            fs::copy(&built_bin, &tmp_dest).and_then(|_| fs::rename(&tmp_dest, &dest));
        if copy_result.is_err() {
            let _ = fs::remove_file(&tmp_dest);
        }

        match copy_result {
            Ok(_) => {
                let susi_name = if cfg!(target_os = "windows") {
                    "susi.exe"
                } else {
                    "susi"
                };
                let susi_dest = bin_dir.join(susi_name);

                // The engine binary is already named `susi`, so the alias
                // target and destination are the same path - removing it
                // here would delete the binary we just installed and leave a
                // self-referencing symlink behind. Only create the alias when
                // the names genuinely differ.
                if susi_dest != dest {
                    let _ = fs::remove_file(&susi_dest);

                    #[cfg(windows)]
                    {
                        let _ = fs::copy(&dest, &susi_dest);
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        let _ = symlink(bin_name, &susi_dest);
                    }
                }

                println!("xtask: Successfully installed latest susi to your local system.");

                // Record how the installed binary was built so a later dev
                // self-install refuses to downgrade it (susi-sandbox
                // `auto_install::BUILD_MARKER`).
                let accelerator = ["cuda", "metal", "mkl"].into_iter().find(|accel| {
                    final_features.iter().any(|arg| {
                        arg.trim_start_matches("--features")
                            .trim_start_matches('=')
                            .split([',', ' '])
                            .any(|feature| feature == *accel)
                    })
                });
                let marker = format!(
                    "{{\"profile\":\"{}\",\"accelerator\":{}}}",
                    profile_dir,
                    accelerator.map_or_else(|| "null".to_string(), |a| format!("\"{a}\""))
                );
                let _ = fs::write(bin_dir.join("susi.build.json"), marker);

                // Seed the host trust anchor so the next `susi start` does not
                // treat a fresh install as a binary-signature change.
                let susi_home = home.join(".susi");
                if let Ok(output) = std::process::Command::new("sha256sum").arg(&dest).output() {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if let Some(hash) = stdout.split_whitespace().next() {
                            let _ = fs::write(susi_home.join("binary.hash"), hash);
                            let _ = fs::remove_file(susi_home.join("binary.hash.cache"));
                            println!("xtask: Updated ~/.susi/binary.hash trust anchor.");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("xtask: Failed to copy binary: {}", e);
                exit(1);
            }
        }
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
