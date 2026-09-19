// susi Launcher: Micro-binary for Exponential Intelligence Substrate Onboarding

use std::process::Command;
use std::env;
use std::path::{Path, PathBuf};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const SUSI_VERSION: &str = env!("CARGO_PKG_VERSION");

fn generate_identity_key(dir: &Path) -> String {
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
    let provisioned = engine_path.exists();
    let target: PathBuf = if provisioned {
        engine_path
    } else {
        // Not provisioned yet: fall back to searching PATH/cwd.
        PathBuf::from(if cfg!(target_os = "windows") {
            "susi-engine.exe"
        } else {
            "susi-engine"
        })
    };

    // Every invocation of this launcher used to fork a child, wait on it,
    // then fall off the end of main() without ever looking at its exit
    // status - susi-engine returning a nonzero (error) exit code was
    // silently swallowed and the launcher always reported success to the
    // shell, breaking `&&`/`$?`/CI exit-code checks. On Unix, exec() avoids
    // this class of bug entirely (it replaces this process's image with
    // susi-engine's, so the real exit code reaches the caller directly, no
    // forwarding needed) and also removes the extra fork+wait indirection.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(&target).args(&args).exec();
        // exec() only returns if it failed to start the process at all.
        eprintln!("Error executing susi-engine: {}", err);
        if !provisioned {
            eprintln!("susi substrate engine not found. Please run 'susi install'.");
        }
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        match Command::new(&target).args(&args).status() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("Error executing susi-engine: {}", e);
                if !provisioned {
                    eprintln!("susi substrate engine not found. Please run 'susi install'.");
                }
                std::process::exit(1);
            }
        }
    }
}
