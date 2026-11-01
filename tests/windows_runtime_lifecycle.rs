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
    for key in ["HOME", "USERPROFILE"] {
        command.env(key, home);
    }
    for (key, relative) in [
        ("SUSI_HOME", ".susi"),
        ("APPDATA", "appdata"),
        ("LOCALAPPDATA", "localappdata"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("TEMP", "temp"),
        ("TMP", "temp"),
    ] {
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
    let result = run(execute, &output, "lifecycle", 600);
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("PASS "))
            .count(),
        12,
        "every lifecycle case must execute: {stdout}"
    );
    let evidence = serde_json::json!({"schema":"susi-cargo-runtime-lifecycle/v1", "passed":true,
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
