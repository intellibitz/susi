// susi Launcher: Micro-binary for Exponential Intelligence Substrate Onboarding

use std::process::Command;
use std::env;
use std::path::PathBuf;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");

fn generate_identity_key(dir: &PathBuf) -> String {
    let key_path = dir.join("identity.key");
    if key_path.exists() {
        return fs::read_to_string(&key_path).unwrap_or_else(|_| "UNKNOWN_ID".to_string());
    }

    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let id = format!("SUSI-SUBSTRATE-{:x}", ts);
    let _ = fs::write(&key_path, &id);
    id
}

fn perform_hardware_audit() {
    println!("\n[HARDWARE INTERROGATION]");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    println!("- Operating System: {}", os);
    println!("- Architecture: {}", arch);

    #[cfg(target_os = "macos")]
    println!("- Neural Acceleration: Apple Metal detected.");

    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/usr/local/cuda").exists() || Command::new("nvcc").status().is_ok() {
            println!("- Neural Acceleration: NVIDIA CUDA detected.");
        } else {
            println!("- Neural Acceleration: Generic CPU Substrate.");
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let global_dir = home.join(".susi");
    let _ = fs::create_dir_all(&global_dir);

    if args.first().map(|s| s.as_str()) == Some("install") {
        println!("susi Substrate Launcher v{}", SUSI_VERSION);
        let id = generate_identity_key(&global_dir);
        perform_hardware_audit();
        println!("\n[SUBSTRATE IDENTITY CARD]");
        println!("- Identity Key: {}", id);
        println!("- Status: Provisioning sandboxed environment...");
    }

    let engine_path = global_dir.join("bin").join("susi-engine");

    if engine_path.exists() {
        let status = Command::new(engine_path)
            .args(&args)
            .status();

        if let Err(e) = status {
            eprintln!("Error executing susi-engine: {}", e);
        }
    } else {
        // If engine not found, try searching in path or current dir
        let fallback = if cfg!(target_os = "windows") { "susi-engine.exe" } else { "susi-engine" };
        let status = Command::new(fallback)
            .args(&args)
            .status();

        if status.is_err() {
            eprintln!("susi substrate engine not found. Please run 'susi install'.");
        }
    }
}
