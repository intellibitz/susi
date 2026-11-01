//! Mandatory native startup qualification against actual Cargo-built runtime libraries.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]

use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn digest(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// This is propagation from the operation's still-live VerifiedSysroot guard,
// never a way for an environment variable to authorize a production release.
fn handoff(
    compiled: Option<&str>,
    runtime: Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    match (compiled, runtime) {
        (Some(_), None) => Err("product lifecycle qualification is missing its verified sysroot handoff".into()),
        (Some(expected), Some(actual)) => {
            let actual=PathBuf::from(actual);
            if !actual.is_absolute() {return Err("sysroot handoff must be absolute".into());}
            let expected=Path::new(expected).canonicalize().map_err(|e|e.to_string())?;
            let actual=actual.canonicalize().map_err(|e|e.to_string())?;
            if actual!=expected {return Err("runtime sysroot differs from the one used to compile the qualification test".into());}
            Ok(Some(actual))
        }
        (None, None) => Ok(None), // Explicit standalone stock-runtime diagnostic.
        (None, Some(_)) => Err("sysroot environment cannot upgrade a stock-built diagnostic into product qualification".into()),
    }
}
fn archive_identity(sysroot: &Path) -> serde_json::Value {
    let directory = sysroot.join("lib/rustlib/x86_64-pc-windows-msvc/lib");
    let mut archives = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        assert!(
            entry.file_type().unwrap().is_file(),
            "sysroot must contain regular archives"
        );
        assert_eq!(
            entry.path().extension().and_then(|v| v.to_str()),
            Some("rlib")
        );
        archives.push(serde_json::json!({"filename":entry.file_name().to_str().unwrap(),"sha256":digest(&entry.path())}));
    }
    archives.sort_by_key(|v| v["filename"].as_str().unwrap().to_owned());
    assert_eq!(
        archives.len(),
        20,
        "verified Windows product sysroot archive set"
    );
    serde_json::Value::Array(archives)
}
fn cargo_sysroot(command: &mut Command, sysroot: Option<&Path>) {
    for name in [
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
    ] {
        command.env(name, "");
    }
    if let Some(path) = sysroot {
        let path = path.to_str().unwrap();
        assert!(!path.contains('\x1f'));
        command.env("CARGO_ENCODED_RUSTFLAGS", format!("--sysroot\x1f{path}"));
        command.env("CARGO_ENCODED_RUSTDOCFLAGS", format!("--sysroot\x1f{path}"));
    }
}
#[test]
fn sysroot_handoff_cannot_be_missing_or_self_authorized() {
    assert!(handoff(Some("compiled sysroot"), None).is_err());
    assert!(handoff(None, Some("caller assertion".into())).is_err());
    assert_eq!(handoff(None, None).unwrap(), None);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        handoff(root.to_str(), Some(root.as_os_str().to_owned())).unwrap(),
        Some(root.canonicalize().unwrap())
    );
    let mut command = Command::new("cargo");
    cargo_sysroot(&mut command, Some(root));
    for key in ["CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS"] {
        assert!(command.get_envs().any(|(name, value)| name == key
            && value
                .unwrap()
                .to_string_lossy()
                .starts_with("--sysroot\x1f")));
    }
}
fn isolated(program: &Path, home: &Path) -> Command {
    let original = PathBuf::from(std::env::var_os("USERPROFILE").expect("native toolchain home"));
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "CREDENTIAL",
                "API_KEY",
                "ACCESS_KEY",
                "BASE_URL",
                "API_BASE",
                "API_URL",
                "PROXY",
                "CUSTOM_HEADERS",
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    for (key, relative) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        command.env(
            key,
            std::env::var_os(key)
                .map(PathBuf::from)
                .unwrap_or_else(|| original.join(relative)),
        );
    }
    susi_paths::test_env::isolate_home(&mut command, home);
    for (key, relative) in [("SUSI_HOME", ".susi"), ("TEMP", "temp"), ("TMP", "temp")] {
        let directory = home.join(relative);
        fs::create_dir_all(&directory).unwrap();
        command.env(key, directory);
    }
    command
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "");
    command
}

fn run(mut command: Command, output: &Path, label: &str, seconds: u64) -> Output {
    let stdout = output.join(format!("{label}.stdout"));
    let stderr = output.join(format!("{label}.stderr"));
    command
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap());
    let mut child = command
        .spawn()
        .expect("native qualification command must start");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(seconds) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "runtime qualification timed out: {label}; evidence {}",
                output.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let result = Output {
        status,
        stdout: fs::read(stdout).unwrap(),
        stderr: fs::read(stderr).unwrap(),
    };
    assert!(
        result.status.success(),
        "{label}: {}\n{}; evidence {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        output.display()
    );
    result
}

#[test]
fn cargo_runtime_preserves_all_twelve_native_lifecycle_cases() {
    let sysroot = handoff(
        option_env!("SUSI_VERIFIED_SYSROOT"),
        std::env::var_os("SUSI_VERIFIED_SYSROOT"),
    )
    .unwrap();
    let before = sysroot.as_deref().map(archive_identity);
    let mode = if sysroot.is_some() {
        "propagated-product-sysroot"
    } else {
        "standalone-stock-sysroot-diagnostic"
    };
    println!("Runtime lifecycle mode: {mode}");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = root.join("crates/susi-windows-runtime");
    let fixture = runtime.join("tests/fixtures/lifecycle.rs");
    let driver_source = runtime.join("tests/fixtures/driver.rs");
    let imports = Path::new(env!("OUT_DIR"));
    let ucrt = imports.join("susi-os-ucrt.lib");
    let stack = imports.join("susi-os-stack.lib");
    for path in [
        runtime.join("Cargo.toml"),
        fixture.clone(),
        driver_source.clone(),
        ucrt.clone(),
        stack.clone(),
    ] {
        assert!(
            path.is_file(),
            "mandatory native runtime input missing: {}",
            path.display()
        );
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = std::env::temp_dir().join(format!(
        "susi-runtime-lifecycle-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&output).unwrap();
    let home = output.join("home");
    fs::create_dir(&home).unwrap();
    let target = output.join("cargo");
    let mut libraries = Vec::new();
    for release in [false, true] {
        let label = if release {
            "cargo-release"
        } else {
            "cargo-debug"
        };
        let mut command = isolated(Path::new(env!("CARGO")), &home);
        cargo_sysroot(&mut command, sysroot.as_deref());
        command
            .current_dir(root)
            .args([
                "build",
                "--package",
                "susi-windows-runtime",
                "--lib",
                "--offline",
                "--locked",
                "--target",
                "x86_64-pc-windows-msvc",
                "--message-format=json-render-diagnostics",
                "--target-dir",
            ])
            .arg(&target)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_TARGET_DIR", &target)
            .env("CARGO_BUILD_BUILD_DIR", &target)
            .env("CARGO_BUILD_TARGET", "x86_64-pc-windows-msvc");
        if release {
            command.arg("--release");
        }
        let result = run(command, &output, label, 120);
        let artifacts: Vec<PathBuf> = String::from_utf8(result.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| {
                row["reason"] == "compiler-artifact"
                    && row["target"]["name"] == "susi_windows_runtime"
            })
            .flat_map(|row| {
                row["filenames"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect::<Vec<_>>()
            })
            .filter(|path| path.extension().is_some_and(|ext| ext == "rlib"))
            .collect();
        assert_eq!(
            artifacts.len(),
            1,
            "Cargo must identify exactly one runtime library"
        );
        assert!(artifacts[0].starts_with(&target));
        libraries.push(artifacts[0].clone());
    }
    let driver = output.join("driver.exe");
    let mut compile = isolated(Path::new("rustc"), &home);
    cargo_sysroot(&mut compile, sysroot.as_deref());
    if let Some(path) = &sysroot {
        compile.arg("--sysroot").arg(path);
    }
    compile
        .args([
            "--edition=2021",
            "--crate-name",
            "susi_runtime_lifecycle_driver",
        ])
        .arg(&driver_source)
        .arg("-o")
        .arg(&driver);
    run(compile, &output, "compile-driver", 120);
    let mut execute = isolated(&driver, &home);
    execute
        .arg(&libraries[0])
        .arg(&ucrt)
        .arg(&stack)
        .arg(&fixture)
        .arg(output.join("cases"))
        .arg(&libraries[1]);
    if let Some(path) = &sysroot {
        execute.arg(path);
    }
    let result = run(execute, &output, "lifecycle", 600);
    assert_eq!(
        before,
        sysroot.as_deref().map(archive_identity),
        "sysroot archives changed during lifecycle qualification"
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("PASS "))
            .count(),
        12,
        "every lifecycle case must execute: {stdout}"
    );
    let evidence = serde_json::json!({"schema":"susi-cargo-runtime-lifecycle/v2", "passed":true,
        "mode":mode,"sysroot":sysroot,"sysroot_archives":before,
        "driver_sha256":digest(&driver_source), "fixture_sha256":digest(&fixture),
        "debug_rlib_sha256":digest(&libraries[0]), "release_rlib_sha256":digest(&libraries[1]),
        "ucrt_import_sha256":digest(&ucrt), "stack_import_sha256":digest(&stack), "cases":stdout});
    fs::write(
        output.join("receipt.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!(
        "Native Cargo-runtime lifecycle evidence: {}",
        output.join("receipt.json").display()
    );
}
