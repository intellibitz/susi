use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

fn get_home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

        // We inject the GPU args into the cargo build invocation
        // But we only want to inject them if they weren't explicitly provided,
        // to stay safe, but for now we just append them.

        let mut final_args = Vec::new();
        for arg in &args {
            final_args.push(arg.clone());
        }
        for arg in gpu_args {
            final_args.push(arg);
        }
        cmd.args(&final_args);

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
            "susi-engine.exe"
        } else {
            "susi-engine"
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

        let _ = fs::remove_file(&dest);

        match fs::copy(&built_bin, &dest) {
            Ok(_) => {
                let susi_name = if cfg!(target_os = "windows") {
                    "susi.exe"
                } else {
                    "susi"
                };
                let susi_dest = bin_dir.join(susi_name);
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

                println!("xtask: Successfully installed latest susi to your local system.");
            }
            Err(e) => {
                eprintln!("xtask: Failed to copy binary: {}", e);
                exit(1);
            }
        }
    }
}
