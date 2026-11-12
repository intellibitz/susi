//! Native exact-tree release qualification (`docs/releases.md`).
//!
//! `cargo run -p xtask --bin release-qualify -- vX.Y.Z [--from-distill <name>]
//! [--jobs N] [--verify-only]`, from a clean checkout of the release tree whose
//! package version is X.Y.Z. With `--from-distill <name>` the tree must be the
//! accept-verified distilled candidate `release/<name>` plus only its version
//! bump; that is checked against the candidate's receipt before any gate runs
//! (`release_qualification::verify_distilled_source`), and the record carries
//! the `distillation` block. Without it the record says `source: snapshot`.
//! `--verify-only` runs every check that precedes the gates and stops there:
//! no gate runs and nothing is written, committed or tagged. It runs every
//! gate in `ALL_QUALIFICATION_GATES` (`REQUIRED_GATES` plus `PERFORMANCE_GATES`:
//! `startup-gate` and the 2000 ms `susi-conformance` host-contract budget) on
//! that exact committed tree, keeps each gate's log
//! under `<target>/release-qualification/<tag>/`, and only when all of them pass
//! writes `releases/qualification/<tag>.json`, commits that one file, checks
//! the commit with the same verifier push-policy uses, and creates the local
//! annotated tag. Pushing the tag (`git push origin vX.Y.Z`) runs push-policy,
//! which admits it only with that record. A failed gate stops the run: nothing
//! is written, committed or tagged.
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
#[cfg(windows)]
extern crate susi_windows_runtime;
#[path = "../harvest.rs"]
mod harvest;
#[path = "../release_qualification.rs"]
mod release_qualification;
#[path = "../release_version_guard.rs"]
mod release_version_guard;
#[allow(dead_code)]
#[path = "../test_runner.rs"]
mod test_runner;

use release_qualification::{
    package_version, record_path, release_version, sha256_hex, toolchain_channel,
    verify_distilled_source, verify_tag, CONFORMANCE_BUDGET_MS, PERFORMANCE_GATES, REQUIRED_GATES,
    SCHEMA, SOURCE_DISTILL, SOURCE_SNAPSHOT,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The binaries a Susi core release ships, as built by the `build` gate.
const ASSETS: [&str; 2] = ["susi", "susi-harness"];

/// Every gate a current `release-qualify` cut executes and records.
const ALL_QUALIFICATION_GATES: [&str; 9] = [
    REQUIRED_GATES[0],
    REQUIRED_GATES[1],
    REQUIRED_GATES[2],
    REQUIRED_GATES[3],
    REQUIRED_GATES[4],
    REQUIRED_GATES[5],
    REQUIRED_GATES[6],
    PERFORMANCE_GATES[0],
    PERFORMANCE_GATES[1],
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(Path::new("."), &args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("release-qualify: {error}");
            ExitCode::FAILURE
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

struct Gate {
    name: &'static str,
    argv: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| (*w).to_owned()).collect()
}

fn gates(jobs: Option<&str>) -> Vec<Gate> {
    let with_jobs = |mut argv: Vec<String>| {
        if let Some(n) = jobs {
            argv.push("-j".into());
            argv.push(n.into());
        }
        argv
    };
    vec![
        Gate {
            name: "format",
            argv: words(&["cargo", "fmt", "--all", "--check"]),
            env: vec![],
        },
        Gate {
            name: "check",
            argv: with_jobs(words(&[
                "cargo",
                "check",
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--offline",
                "--locked",
            ])),
            env: vec![],
        },
        Gate {
            name: "clippy",
            argv: {
                let mut argv = with_jobs(words(&[
                    "cargo",
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--offline",
                    "--locked",
                ]));
                argv.extend(words(&["--", "-D", "warnings"]));
                argv
            },
            env: vec![],
        },
        Gate {
            name: "licenses",
            argv: words(&[
                "cargo",
                "deny",
                "--offline",
                "--locked",
                "check",
                "licenses",
            ]),
            env: vec![],
        },
        Gate {
            name: "tests",
            argv: with_jobs(words(&[
                "cargo",
                "test",
                "--workspace",
                "--offline",
                "--locked",
                "--no-fail-fast",
            ])),
            env: vec![],
        },
        Gate {
            name: "build",
            argv: with_jobs(words(&[
                "cargo",
                "build",
                "--offline",
                "--locked",
                "--profile",
                "dist",
                "-p",
                "susi-harness",
                "-p",
                "susi",
                "--features",
                "susi/local-engine",
            ])),
            env: vec![
                ("CARGO_PROFILE_DIST_LTO", "false"),
                ("CARGO_PROFILE_DIST_CODEGEN_UNITS", "16"),
            ],
        },
    ]
}

/// Sum of every `test result:` line: binaries, passed, failed, ignored.
fn test_counts(log: &str) -> Value {
    let (mut binaries, mut passed, mut failed, mut ignored) = (0u64, 0u64, 0u64, 0u64);
    for line in log.lines() {
        let Some(rest) = line.strip_prefix("test result: ") else {
            continue;
        };
        binaries += 1;
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
    json!({"binaries": binaries, "passed": passed, "failed": failed, "ignored": ignored})
}

fn run_gate(root: &Path, logs: &Path, gate: &Gate) -> Result<Value, String> {
    let log_path = logs.join(format!("{}.log", gate.name));
    let log =
        std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let (program, args) = gate.argv.split_first().ok_or("empty gate command")?;
    eprintln!("release-qualify: {} ...", gate.name);
    let started = Instant::now();
    let status = Command::new(program)
        .args(args)
        .envs(gate.env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .status()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    let seconds = started.elapsed().as_secs();
    let bytes = std::fs::read(&log_path).map_err(|e| e.to_string())?;
    let code = status.code().unwrap_or(-1);
    eprintln!("release-qualify: {} exit {code} in {seconds} s", gate.name);
    let mut record = json!({
        "name": gate.name,
        "argv": gate.argv,
        "env": gate.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "exit_code": code,
        "duration_s": seconds,
        "log_sha256": sha256_hex(&bytes),
    });
    if gate.name == "tests" {
        record["tests"] = test_counts(&String::from_utf8_lossy(&bytes));
    }
    if code != 0 {
        return Err(format!(
            "gate `{}` failed (exit {code}); log: {}",
            gate.name,
            log_path.display()
        ));
    }
    Ok(record)
}

/// Concurrent binary lanes, total Rust test threads, and per-binary build jobs
/// for `release-qualify`'s `tests` gate.
fn parallel_test_concurrency(jobs: Option<&str>, available: usize) -> (usize, usize, usize) {
    let jobs_n = jobs
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(available)
        .max(1);
    let concurrency = jobs_n.clamp(2, 12);
    let total_test_threads = (concurrency * 2).max(jobs_n);
    let build_jobs = (jobs_n / concurrency).max(1);
    (concurrency, total_test_threads, build_jobs)
}

fn resolve_target_dir_from(root: &Path, cargo_target_dir: Option<&std::ffi::OsStr>) -> PathBuf {
    cargo_target_dir
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { root.join(p) })
        .unwrap_or_else(|| root.join("target"))
}

fn resolve_target_dir(root: &Path) -> PathBuf {
    resolve_target_dir_from(root, std::env::var_os("CARGO_TARGET_DIR").as_deref())
}

struct CargoEnv {
    cargo_home: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    target_dir: PathBuf,
}

fn capture_cargo_env(root: &Path) -> CargoEnv {
    let original_home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".cargo")));
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|h| h.join(".rustup")));
    let target_dir = resolve_target_dir(root);
    CargoEnv {
        cargo_home,
        rustup_home,
        target_dir,
    }
}

fn apply_isolated_cargo_env(cmd: &mut Command, private_home: &Path, cargo_env: &CargoEnv) {
    susi_paths::test_env::isolate_home(cmd, private_home);
    cmd.env("SUSI_NO_REGISTRY_ENV", "1");
    if let Some(ref ch) = cargo_env.cargo_home {
        cmd.env("CARGO_HOME", ch);
    }
    if let Some(ref rh) = cargo_env.rustup_home {
        cmd.env("RUSTUP_HOME", rh);
    }
    cmd.env("CARGO_TARGET_DIR", &cargo_env.target_dir);
}

fn run_parallel_tests_gate(
    root: &Path,
    logs: &Path,
    gate: &Gate,
    jobs: Option<&str>,
) -> Result<Value, String> {
    let log_path = logs.join(format!("{}.log", gate.name));
    eprintln!("release-qualify: {} (parallel) ...", gate.name);
    let started = Instant::now();

    let cargo_env = capture_cargo_env(root);

    let mut build_args = words(&[
        "test",
        "--workspace",
        "--offline",
        "--locked",
        "--no-run",
        "--message-format=json",
    ]);
    if let Some(n) = jobs {
        build_args.push("-j".into());
        build_args.push(n.into());
    }
    let build_out = Command::new("cargo")
        .args(&build_args)
        .envs(gate.env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start cargo test --no-run: {e}"))?;
    let stdout_text = String::from_utf8_lossy(&build_out.stdout);
    let parsed = test_runner::parse_artifacts(&stdout_text).map_err(|e| e.to_string());
    let mut initial_log = Vec::new();
    initial_log.extend_from_slice(&build_out.stderr);
    if let Ok((_, ref diagnostics)) = parsed {
        for line in diagnostics {
            initial_log.extend_from_slice(line.as_bytes());
            initial_log.push(b'\n');
        }
    } else {
        initial_log.extend_from_slice(&build_out.stdout);
    }
    std::fs::write(&log_path, &initial_log).map_err(|e| format!("{}: {e}", log_path.display()))?;
    if !build_out.status.success() {
        let code = build_out.status.code().unwrap_or(1);
        return Err(format!(
            "gate `{}` failed (exit {code}); log: {}",
            gate.name,
            log_path.display()
        ));
    }
    let (binaries, _) = parsed?;

    let avail = std::thread::available_parallelism().map_or(4, usize::from);
    let (concurrency, total_test_threads, build_jobs) = parallel_test_concurrency(jobs, avail);
    let home_root = logs.join("test-homes");
    let durations = logs.join("test-durations.json");
    let heavy = test_runner::heavy_binaries(root);
    let settings = test_runner::Settings {
        concurrency,
        total_test_threads,
        build_jobs,
        home_root: &home_root,
        target_dir: &cargo_env.target_dir,
        triple: None,
        durations: &durations,
        heavy: &heavy,
        log: &log_path,
    };
    let outcome = test_runner::run(&binaries, &settings, &|binary, private_home| {
        let mut child = Command::new(&binary.executable);
        child.envs(gate.env.iter().copied());
        susi_paths::test_env::isolate_home(&mut child, private_home);
        if let Some(ref ch) = cargo_env.cargo_home {
            child.env("CARGO_HOME", ch);
        }
        if let Some(ref rh) = cargo_env.rustup_home {
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
    .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&home_root);
    let outcome = outcome?;

    let mut doc_args = words(&[
        "test",
        "--workspace",
        "--doc",
        "--offline",
        "--locked",
        "--no-fail-fast",
    ]);
    if let Some(n) = jobs {
        doc_args.push("-j".into());
        doc_args.push(n.into());
    }
    let doc_home = logs.join("doc-home");
    let _ = std::fs::remove_dir_all(&doc_home);
    std::fs::create_dir_all(&doc_home).map_err(|e| e.to_string())?;
    let log_append = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("{}: {e}", log_path.display()))?;
    let err_append = log_append.try_clone().map_err(|e| e.to_string())?;
    let mut doc_cmd = Command::new("cargo");
    doc_cmd
        .args(&doc_args)
        .envs(gate.env.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(log_append)
        .stderr(err_append);
    susi_paths::test_env::isolate_home(&mut doc_cmd, &doc_home);
    if let Some(ref ch) = cargo_env.cargo_home {
        doc_cmd.env("CARGO_HOME", ch);
    }
    if let Some(ref rh) = rustup_home_ref(&cargo_env) {
        doc_cmd.env("RUSTUP_HOME", rh);
    }
    let doc_status = doc_cmd
        .status()
        .map_err(|e| format!("cannot start cargo test --doc: {e}"))?;
    let _ = std::fs::remove_dir_all(&doc_home);

    let seconds = started.elapsed().as_secs();
    let bytes = std::fs::read(&log_path).map_err(|e| e.to_string())?;
    let binaries_ok = outcome.failed.is_empty() && outcome.ran == binaries.len();
    let code = if !binaries_ok {
        1
    } else if !doc_status.success() {
        doc_status.code().unwrap_or(1)
    } else {
        0
    };
    eprintln!(
        "release-qualify: {} exit {code} in {seconds} s ({} binaries across {concurrency} lanes + doctests)",
        gate.name,
        binaries.len()
    );
    let record = json!({
        "name": gate.name,
        "argv": gate.argv,
        "env": gate.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
        "exit_code": code,
        "duration_s": seconds,
        "log_sha256": sha256_hex(&bytes),
        "tests": test_counts(&String::from_utf8_lossy(&bytes)),
    });
    if code != 0 {
        return Err(format!(
            "gate `{}` failed (exit {code}); log: {}",
            gate.name,
            log_path.display()
        ));
    }
    Ok(record)
}

fn rustup_home_ref(cargo_env: &CargoEnv) -> Option<&Path> {
    cargo_env.rustup_home.as_deref()
}

/// Each shipped binary answers `--version` with the release version, from a
/// private home so no operator state is read or written.
fn smoke(target_dir: &Path, logs: &Path, version: &str) -> Result<(Value, Vec<Value>), String> {
    let home = logs.join("smoke-home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let mut assets = Vec::new();
    let mut argv = Vec::new();
    let started = Instant::now();
    for name in ASSETS {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let path = target_dir.join("dist").join(&file);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cmd = Command::new(&path);
        susi_paths::test_env::isolate_home(&mut cmd, &home);
        let out = cmd
            .arg("--version")
            .env("SUSI_HOME", home.join(".susi"))
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .current_dir(&home)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {file}: {e}"))?;
        let answer = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || !answer.contains(version) {
            let _ = std::fs::remove_dir_all(&home);
            return Err(format!(
                "smoke: {file} --version answered {answer:?} ({}), expected {version}",
                out.status
            ));
        }
        argv.push(format!("target/dist/{file} --version"));
        assets.push(json!({
            "file": file,
            "bytes": bytes.len(),
            "sha256": sha256_hex(&bytes),
            "version_output": answer,
        }));
    }
    let _ = std::fs::remove_dir_all(&home);
    let gate = json!({
        "name": "smoke",
        "argv": argv,
        "env": ["HOME=<private>", "SUSI_HOME=<private>", "SUSI_NO_REGISTRY_ENV=1"],
        "exit_code": 0,
        "duration_s": started.elapsed().as_secs(),
    });
    Ok((gate, assets))
}

fn dist_susi_binary(target_dir: &Path) -> PathBuf {
    target_dir
        .join("dist")
        .join(format!("susi{}", std::env::consts::EXE_SUFFIX))
}

fn summarize_startup_gate_json(stdout: &[u8]) -> Result<(Value, String), String> {
    let report: Value = serde_json::from_slice(stdout)
        .map_err(|e| format!("startup-gate stdout is not JSON: {e}"))?;
    if report["pass"].as_bool() != Some(true) {
        return Err("startup-gate reported `pass != true`".into());
    }
    let schema = report["schema"]
        .as_str()
        .unwrap_or("susi-startup-gate-v2")
        .to_owned();
    let summary = json!({
        "schema": schema,
        "pass": true,
        "floor_ms": report["floor"]["median_ms"],
    });
    Ok((summary, schema))
}

/// Run `startup-gate` against the built `dist/susi` executable in an isolated
/// private home so user-response budgets are proven before tagging.
fn run_startup_gate(
    root: &Path,
    target_dir: &Path,
    logs: &Path,
    jobs: Option<&str>,
) -> Result<(Value, String), String> {
    let dist_susi = dist_susi_binary(target_dir);
    let recorded_susi = format!("target/dist/susi{}", std::env::consts::EXE_SUFFIX);
    let startup_home = logs.join("startup-home");
    let _ = std::fs::remove_dir_all(&startup_home);
    let log_path = logs.join("startup.log");

    let mut base_argv = words(&[
        "cargo",
        "run",
        "--offline",
        "--locked",
        "-p",
        "xtask",
        "--bin",
        "startup-gate",
    ]);
    if let Some(n) = jobs {
        base_argv.push("-j".into());
        base_argv.push(n.into());
    }
    let mut exec_argv = base_argv.clone();
    exec_argv.extend([
        "--".to_owned(),
        "--susi".to_owned(),
        dist_susi.display().to_string(),
        "--home".to_owned(),
        startup_home.display().to_string(),
        "--runs".to_owned(),
        "5".to_owned(),
    ]);
    let mut recorded_argv = base_argv;
    recorded_argv.extend([
        "--".to_owned(),
        "--susi".to_owned(),
        recorded_susi,
        "--home".to_owned(),
        "<private>/startup-home".to_owned(),
        "--runs".to_owned(),
        "5".to_owned(),
    ]);

    let cargo_env = capture_cargo_env(root);
    let mut cmd = Command::new("cargo");
    cmd.args(exec_argv.get(1..).unwrap_or_default())
        .current_dir(root)
        .stdin(Stdio::null());
    apply_isolated_cargo_env(&mut cmd, &startup_home, &cargo_env);

    eprintln!("release-qualify: startup ...");
    let started = Instant::now();
    let out = cmd
        .output()
        .map_err(|e| format!("cannot start startup-gate: {e}"))?;
    let seconds = started.elapsed().as_secs();
    let _ = std::fs::remove_dir_all(&startup_home);

    let mut log_bytes = Vec::with_capacity(out.stderr.len() + out.stdout.len());
    log_bytes.extend_from_slice(&out.stderr);
    log_bytes.extend_from_slice(&out.stdout);
    std::fs::write(&log_path, &log_bytes).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let code = out.status.code().unwrap_or(-1);
    eprintln!("release-qualify: startup exit {code} in {seconds} s");
    if code != 0 {
        return Err(format!(
            "gate `startup` failed (exit {code}); log: {}",
            log_path.display()
        ));
    }
    let (startup_summary, schema) = summarize_startup_gate_json(&out.stdout)
        .map_err(|e| format!("gate `startup` failed ({e}); log: {}", log_path.display()))?;
    let record = json!({
        "name": "startup",
        "argv": recorded_argv,
        "env": ["HOME=<private>", "SUSI_HOME=<private>", "SUSI_NO_REGISTRY_ENV=1"],
        "exit_code": code,
        "duration_s": seconds,
        "log_sha256": sha256_hex(&log_bytes),
        "startup_gate": startup_summary,
    });
    Ok((record, schema))
}

const CONFORMANCE_INSTANCE_PORTS: [u16; 17] = [
    9090, 9091, 9092, 9093, 9094, 9095, 9180, 9181, 18080, 18081, 18082, 18083, 18084, 19090,
    19091, 19092, 19093,
];

fn conformance_offset_free(offset: u16) -> bool {
    (9090u16..=9094).all(|base| {
        base.checked_add(offset)
            .is_some_and(|port| std::net::TcpListener::bind(("127.0.0.1", port)).is_ok())
    }) && CONFORMANCE_INSTANCE_PORTS.iter().all(|base| {
        base.checked_add(offset).is_some_and(|port| {
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
                && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        })
    })
}

fn pick_conformance_port_offset() -> Option<u16> {
    let candidates: Vec<u16> = (14_000u16..24_000u16).step_by(10).collect();
    if candidates.is_empty() {
        return None;
    }
    let start = (std::process::id() as usize) % candidates.len();
    (0..candidates.len())
        .filter_map(|i| candidates.get((start + i) % candidates.len()).copied())
        .find(|offset| conformance_offset_free(*offset))
}

fn http_health_ready(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 256];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
}

/// Ensures the spawned isolated daemon is cleanly stopped, killed if needed,
/// reaped, and its throwaway home removed on every exit path.
struct ScopedConformanceDaemon {
    child: std::process::Child,
    susi_bin: PathBuf,
    home: PathBuf,
    instance: PathBuf,
    offset: u16,
}

impl ScopedConformanceDaemon {
    fn shutdown(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let mut stop = Command::new(&self.susi_bin);
            susi_paths::test_env::isolate_home(&mut stop, &self.home);
            stop.arg("stop")
                .env("SUSI_HOME", &self.instance)
                .env("SUSI_PORT_OFFSET", self.offset.to_string())
                .env("SUSI_NO_REGISTRY_ENV", "1")
                .current_dir(&self.home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(mut stopper) = stop.spawn() {
                let deadline = Instant::now() + Duration::from_secs(15);
                while matches!(stopper.try_wait(), Ok(None)) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
                let _ = stopper.kill();
                let _ = stopper.wait();
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

impl Drop for ScopedConformanceDaemon {
    fn drop(&mut self) {
        self.shutdown();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn summarize_conformance_json(stdout: &[u8], budget_ms: u64) -> Result<(Value, u64, u64), String> {
    let report: Value = serde_json::from_slice(stdout)
        .map_err(|e| format!("susi-conformance stdout is not JSON: {e}"))?;
    if report["conformant"].as_bool() != Some(true) {
        return Err("susi-conformance reported `conformant != true`".into());
    }
    let checks = report["checks"]
        .as_array()
        .ok_or("susi-conformance output has no `checks` array")?;
    if checks.is_empty() {
        return Err("susi-conformance recorded zero checks".into());
    }
    let check_count = u64::try_from(checks.len()).unwrap_or(0);
    let mut max_check_ms = 0u64;
    for check in checks {
        let ms = check["ms"]
            .as_u64()
            .ok_or("susi-conformance check has no integer `ms`")?;
        if ms > max_check_ms {
            max_check_ms = ms;
        }
    }
    if max_check_ms > budget_ms {
        return Err(format!(
            "susi-conformance max_check_ms ({max_check_ms} ms) exceeds {budget_ms} ms budget"
        ));
    }
    let summary = json!({
        "conformant": true,
        "budget_ms": budget_ms,
        "max_check_ms": max_check_ms,
        "checks": check_count,
    });
    Ok((summary, max_check_ms, check_count))
}

/// Spawn `dist/susi daemon-start` in an isolated private home on an ephemeral
/// high `SUSI_PORT_OFFSET` and run `susi-conformance --max-ms 2000 --susi-bin`
/// against it.
fn run_conformance_gate(
    root: &Path,
    target_dir: &Path,
    logs: &Path,
    jobs: Option<&str>,
) -> Result<(Value, u64, u64), String> {
    let dist_susi = dist_susi_binary(target_dir);
    let recorded_susi = format!("target/dist/susi{}", std::env::consts::EXE_SUFFIX);
    let offset = pick_conformance_port_offset()
        .ok_or("no free SUSI_PORT_OFFSET in 14000..24000 for conformance gate")?;
    let offset_str = offset.to_string();
    let budget_str = CONFORMANCE_BUDGET_MS.to_string();

    let conformance_home = logs.join("conformance-home");
    let _ = std::fs::remove_dir_all(&conformance_home);
    let instance = conformance_home.join("instance");
    std::fs::create_dir_all(&instance).map_err(|e| format!("{}: {e}", instance.display()))?;
    let daemon_stderr_path = conformance_home.join("daemon.stderr");
    let daemon_stderr = std::fs::File::create(&daemon_stderr_path)
        .map_err(|e| format!("{}: {e}", daemon_stderr_path.display()))?;
    let log_path = logs.join("conformance.log");

    eprintln!("release-qualify: conformance (SUSI_PORT_OFFSET={offset}) ...");
    let started = Instant::now();

    let mut daemon_cmd = Command::new(&dist_susi);
    susi_paths::test_env::isolate_home(&mut daemon_cmd, &conformance_home);
    daemon_cmd
        .arg("daemon-start")
        .arg("--workspace")
        .arg(&instance)
        .env("SUSI_HOME", &instance)
        .env("SUSI_PORT_OFFSET", &offset_str)
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .current_dir(&conformance_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(daemon_stderr);
    let child = daemon_cmd
        .spawn()
        .map_err(|e| format!("cannot start {} daemon-start: {e}", dist_susi.display()))?;
    let daemon_pid = child.id();
    let mut daemon = ScopedConformanceDaemon {
        child,
        susi_bin: dist_susi.clone(),
        home: conformance_home.clone(),
        instance: instance.clone(),
        offset,
    };

    let ports_file = instance.join("ports.json");
    let readiness_deadline = Instant::now() + Duration::from_secs(60);
    let token_path = loop {
        match daemon.child.try_wait() {
            Ok(Some(status)) => {
                let daemon_log = std::fs::read(&daemon_stderr_path).unwrap_or_default();
                let _ = std::fs::write(&log_path, &daemon_log);
                return Err(format!(
                    "gate `conformance` daemon exited early ({status}); log: {}",
                    log_path.display()
                ));
            }
            Ok(None) => {}
            Err(e) => {
                return Err(format!("gate `conformance` try_wait failed: {e}"));
            }
        }
        let maybe_token = std::fs::read_to_string(&ports_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .filter(|v| v["pid"].as_u64() == Some(u64::from(daemon_pid)))
            .map(|v| {
                v["api_token_path"]
                    .as_str()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| instance.join("api_token"))
            })
            .filter(|tok_path| {
                std::fs::read_to_string(tok_path)
                    .map(|t| !t.trim().is_empty())
                    .unwrap_or(false)
            });
        if let Some(tok_path) = maybe_token {
            let all_healthy = [9090u16, 9091, 9093, 9094]
                .into_iter()
                .all(|base| http_health_ready(base.saturating_add(offset)));
            if all_healthy {
                break tok_path;
            }
        }
        if Instant::now() >= readiness_deadline {
            let daemon_log = std::fs::read(&daemon_stderr_path).unwrap_or_default();
            let _ = std::fs::write(&log_path, &daemon_log);
            return Err(format!(
                "gate `conformance` daemon pid {daemon_pid} did not become ready within 60 s; log: {}",
                log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let mut base_argv = words(&[
        "cargo",
        "run",
        "--offline",
        "--locked",
        "-p",
        "susi-conformance",
    ]);
    if let Some(n) = jobs {
        base_argv.push("-j".into());
        base_argv.push(n.into());
    }
    let mut exec_argv = base_argv.clone();
    exec_argv.extend([
        "--".to_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port-offset".to_owned(),
        offset_str.clone(),
        "--token-file".to_owned(),
        token_path.display().to_string(),
        "--max-ms".to_owned(),
        budget_str.clone(),
        "--susi-bin".to_owned(),
        dist_susi.display().to_string(),
        "--json".to_owned(),
    ]);
    let mut recorded_argv = base_argv;
    recorded_argv.extend([
        "--".to_owned(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "--port-offset".to_owned(),
        offset_str.clone(),
        "--token-file".to_owned(),
        "<private>/api_token".to_owned(),
        "--max-ms".to_owned(),
        budget_str,
        "--susi-bin".to_owned(),
        recorded_susi,
        "--json".to_owned(),
    ]);

    let cargo_env = capture_cargo_env(root);
    let mut conf_cmd = Command::new("cargo");
    conf_cmd
        .args(exec_argv.get(1..).unwrap_or_default())
        .current_dir(root)
        .stdin(Stdio::null());
    apply_isolated_cargo_env(&mut conf_cmd, &conformance_home, &cargo_env);
    conf_cmd
        .env("SUSI_HOME", &instance)
        .env("SUSI_PORT_OFFSET", &offset_str);

    let out = conf_cmd
        .output()
        .map_err(|e| format!("cannot start susi-conformance: {e}"))?;
    daemon.shutdown();
    let daemon_err = std::fs::read(&daemon_stderr_path).unwrap_or_default();
    drop(daemon);

    let seconds = started.elapsed().as_secs();
    let mut log_bytes =
        Vec::with_capacity(daemon_err.len() + out.stderr.len() + out.stdout.len() + 2);
    log_bytes.extend_from_slice(&daemon_err);
    log_bytes.extend_from_slice(&out.stderr);
    log_bytes.extend_from_slice(&out.stdout);
    std::fs::write(&log_path, &log_bytes).map_err(|e| format!("{}: {e}", log_path.display()))?;

    let code = out.status.code().unwrap_or(-1);
    eprintln!("release-qualify: conformance exit {code} in {seconds} s");
    if code != 0 {
        return Err(format!(
            "gate `conformance` failed (exit {code}); log: {}",
            log_path.display()
        ));
    }
    let (conformance_summary, max_check_ms, check_count) =
        summarize_conformance_json(&out.stdout, CONFORMANCE_BUDGET_MS).map_err(|e| {
            format!(
                "gate `conformance` failed ({e}); log: {}",
                log_path.display()
            )
        })?;
    let record = json!({
        "name": "conformance",
        "argv": recorded_argv,
        "env": [
            "HOME=<private>",
            "SUSI_HOME=<private>",
            "SUSI_NO_REGISTRY_ENV=1",
            format!("SUSI_PORT_OFFSET={offset}"),
        ],
        "exit_code": code,
        "duration_s": seconds,
        "log_sha256": sha256_hex(&log_bytes),
        "conformance": conformance_summary,
    });
    Ok((record, max_check_ms, check_count))
}

const USAGE: &str =
    "usage: release-qualify vX.Y.Z [--from-distill <candidate>] [--jobs N] [--verify-only] [--remote <name>] [--skip-remote-check]";

/// The parsed command line.
struct Options {
    tag: String,
    jobs: Option<String>,
    from_distill: Option<String>,
    verify_only: bool,
    /// The storage remote the version is checked and reserved on.
    remote: String,
    /// Whether `--remote` was explicitly supplied on the command line.
    explicit_remote: bool,
    /// Qualify without asking the remote (it cannot be reached).
    skip_remote_check: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut tag = None;
    let mut jobs = None;
    let mut from_distill = None;
    let mut verify_only = false;
    let mut remote = None;
    let mut skip_remote_check = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--jobs" if jobs.is_none() => jobs = Some(args.next().ok_or(USAGE)?.clone()),
            "--from-distill" if from_distill.is_none() => {
                from_distill = Some(args.next().ok_or(USAGE)?.clone());
            }
            "--verify-only" if !verify_only => verify_only = true,
            "--remote" if remote.is_none() => remote = Some(args.next().ok_or(USAGE)?.clone()),
            "--skip-remote-check" if !skip_remote_check => skip_remote_check = true,
            other if !other.starts_with('-') && tag.is_none() => tag = Some(other.to_owned()),
            _ => return Err(USAGE.into()),
        }
    }
    let explicit_remote = remote.is_some();
    Ok(Options {
        tag: tag.ok_or(USAGE)?,
        jobs,
        from_distill,
        verify_only,
        remote: remote.unwrap_or_else(|| "origin".to_owned()),
        explicit_remote,
        skip_remote_check,
    })
}

/// Local refs that name the candidate, checked against its receipt: the
/// immutable snapshot tag `susi-distill/<name>` must be the composed commit,
/// the immutable tag `susi-distill/legacy-release-<name>` (which preserved the
/// envelopes of the release branches retired from the Git host) must be the
/// envelope, and the local-only release branch must contain the envelope.
fn corroborate(root: &Path, distillation: &Value) -> Result<Vec<String>, String> {
    let field = |name: &str| distillation[name].as_str().unwrap_or_default().to_owned();
    let (name, envelope, composed) = (
        field("candidate"),
        field("envelope_commit"),
        field("final_commit"),
    );
    let resolve = |reference: &str| {
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{reference}^{{commit}}"),
            ],
        )
        .ok()
        .filter(|id| !id.is_empty())
    };
    let mut seen = Vec::new();
    let snapshot = format!("refs/tags/susi-distill/{name}");
    if let Some(id) = resolve(&snapshot) {
        if id != composed {
            return Err(format!(
                "{snapshot} is {id}, but the receipt's composed commit is {composed}"
            ));
        }
        seen.push(snapshot);
    }
    let preserved = format!("refs/tags/susi-distill/legacy-release-{name}");
    if let Some(id) = resolve(&preserved) {
        if id != envelope {
            return Err(format!(
                "{preserved} is {id}, but the receipt's envelope commit is {envelope}"
            ));
        }
        seen.push(preserved);
    }
    let local = format!("refs/heads/release/{name}");
    if let Some(id) = resolve(&local) {
        if git(root, &["merge-base", "--is-ancestor", &envelope, &id]).is_err() {
            return Err(format!(
                "{local} ({id}) does not contain the envelope commit {envelope}"
            ));
        }
        seen.push(local);
    }
    Ok(seen)
}

fn run(cwd: &Path, args: &[String]) -> Result<String, String> {
    let Options {
        tag,
        jobs,
        from_distill,
        verify_only,
        remote,
        explicit_remote,
        skip_remote_check,
    } = parse(args)?;
    let version = release_version(&tag).ok_or(USAGE)?.to_owned();
    let root = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?);
    if !git(&root, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("the checkout has uncommitted changes; qualify a committed tree only".into());
    }
    let path = record_path(&tag);
    if root.join(&path).exists() {
        return Err(format!("{path} already exists in this tree"));
    }
    if !git(&root, &["tag", "--list", &tag])?.is_empty() {
        return Err(format!("tag {tag} already exists"));
    }
    let commit = git(&root, &["rev-parse", "HEAD"])?;
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"])?;
    let read = |file: &str| std::fs::read(root.join(file)).map_err(|e| format!("{file}: {e}"));
    let manifest = String::from_utf8_lossy(&read("Cargo.toml")?).into_owned();
    if package_version(&manifest).as_deref() != Some(version.as_str()) {
        return Err(format!("Cargo.toml is not version {version}"));
    }
    let lock_sha = sha256_hex(&read("Cargo.lock")?);
    let channel = toolchain_channel(&String::from_utf8_lossy(&read("rust-toolchain.toml")?))
        .ok_or("rust-toolchain.toml names no channel")?;
    let rustc = Command::new("rustc")
        .arg("-vV")
        .current_dir(&root)
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    let rustc = String::from_utf8_lossy(&rustc.stdout).into_owned();
    let host = rustc
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or("rustc -vV names no host")?
        .to_owned();
    let release = rustc.lines().next().unwrap_or_default().to_owned();
    if !release.contains(&channel) {
        return Err(format!(
            "active compiler {release} is not the pinned {channel}"
        ));
    }
    let (source, distillation, corroborated) = match &from_distill {
        None => (SOURCE_SNAPSHOT, None, Vec::new()),
        Some(name) => {
            let block = verify_distilled_source(&root, name, &commit)
                .map_err(|e| format!("--from-distill {name} refused: {e}"))?;
            let seen = corroborate(&root, &block)
                .map_err(|e| format!("--from-distill {name} refused: {e}"))?;
            (SOURCE_DISTILL, Some(block), seen)
        }
    };
    let harvest_range = match &distillation {
        Some(block) => match block["base_commit"].as_str() {
            Some(base) if !base.is_empty() && base != commit => format!("{base}..{commit}"),
            _ => commit.clone(),
        },
        None => commit.clone(),
    };
    let harvest_raw = harvest::run(
        &root,
        &[
            "audit".to_owned(),
            "--range".to_owned(),
            harvest_range,
            "--json".to_owned(),
        ],
    )
    .map_err(|e| format!("harvest audit refused: {e}"))?;
    let harvest_audit: Value = serde_json::from_str(&harvest_raw)
        .map_err(|e| format!("harvest audit output is not JSON: {e}"))?;
    if verify_only {
        let has_remote_check = if skip_remote_check {
            false
        } else if explicit_remote {
            true
        } else {
            !git(&root, &["remote"])?.is_empty()
        };
        if has_remote_check {
            release_version_guard::check(&root, &remote, &tag, &commit)?;
        }
        let mut summary = format!(
            "verify-only: {tag} from {source} tree {tree} (commit {commit}) passes every check before the gates (including local-harvest audit); no gate ran, nothing was written, committed or tagged."
        );
        if let Some(block) = &distillation {
            summary.push_str(&format!(
                "\ndistillation: {}\ncorroborated by: {}",
                serde_json::to_string_pretty(block).map_err(|e| e.to_string())?,
                if corroborated.is_empty() {
                    "no local candidate ref".to_owned()
                } else {
                    corroborated.join(", ")
                }
            ));
        }
        return Ok(summary);
    }
    // One version, one cut: reserve it on the storage remote before the
    // gates run, and give it back if they fail.
    let agent = std::env::var("SUSI_AGENT")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| git(&root, &["config", "user.name"]).ok())
        .unwrap_or_else(|| "unknown".to_owned());
    if !skip_remote_check {
        release_version_guard::reserve(
            &root,
            &remote,
            &tag,
            &commit,
            &release_version_guard::Holder {
                agent: &agent,
                host: &host,
                now: now(),
            },
        )?;
    }
    let target_dir = resolve_target_dir(&root);
    let logs = target_dir.join("release-qualification").join(&tag);
    let started = now();
    struct GatePhaseOutcome {
        records: Vec<Value>,
        assets: Vec<Value>,
        performance_gates: Value,
    }
    let gate_phase = (|| -> Result<GatePhaseOutcome, String> {
        std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
        let mut records = Vec::new();
        for gate in gates(jobs.as_deref()) {
            let step = if gate.name == "tests" && !test_runner::serial_requested() {
                run_parallel_tests_gate(&root, &logs, &gate, jobs.as_deref())
            } else {
                run_gate(&root, &logs, &gate)
            };
            records.push(step?);
        }
        let (smoke_gate, assets) = smoke(&target_dir, &logs, &version)?;
        records.push(smoke_gate);
        let (startup_gate, startup_schema) =
            run_startup_gate(&root, &target_dir, &logs, jobs.as_deref())?;
        records.push(startup_gate);
        let (conformance_gate, conformance_max_check_ms, conformance_checks) =
            run_conformance_gate(&root, &target_dir, &logs, jobs.as_deref())?;
        records.push(conformance_gate);
        let names: Vec<&str> = records.iter().filter_map(|g| g["name"].as_str()).collect();
        if ALL_QUALIFICATION_GATES
            .iter()
            .any(|required| !names.contains(required))
        {
            return Err("a required gate did not run".into());
        }
        let performance_gates = json!({
            "ok": true,
            "startup_gate_schema": startup_schema,
            "conformance_budget_ms": CONFORMANCE_BUDGET_MS,
            "conformance_max_check_ms": conformance_max_check_ms,
            "conformance_checks": conformance_checks,
        });
        Ok(GatePhaseOutcome {
            records,
            assets,
            performance_gates,
        })
    })();
    let GatePhaseOutcome {
        records,
        assets,
        performance_gates,
    } = match gate_phase {
        Ok(done) => done,
        Err(error) => {
            if !skip_remote_check {
                release_version_guard::release(&root, &remote, &tag, &commit, &agent);
            }
            return Err(error);
        }
    };
    let owed: Vec<&str> = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
        .into_iter()
        .filter(|target| *target != host)
        .collect();
    let mut record = json!({
        "schema": SCHEMA,
        "product": "susi",
        "tag": tag,
        "version": version,
        "qualified_commit": commit,
        "qualified_tree": tree,
        "cargo_lock_sha256": lock_sha,
        "toolchain": {"channel": channel, "rustc": release},
        "host": host,
        "started_unix": started,
        "finished_unix": now(),
        "gates": records,
        "assets": assets,
        "owed_platforms": owed,
        "provenance": "written by xtask release-qualify from the logs under target/release-qualification; each gate's log digest is recorded",
        "source": source,
        "harvest_audit": harvest_audit,
        "performance_gates": performance_gates,
    });
    if let Some(block) = &distillation {
        record["distillation"] = block.clone();
    }
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())? + "\n";
    let file = root.join(&path);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, &text).map_err(|e| e.to_string())?;
    git(&root, &["add", "--", &path])?;
    let subject = format!("chore: release {tag} qualification record");
    let origin = match &distillation {
        Some(block) => format!(
            "distilled candidate {} (receipt sha256 {}, envelope {}, tasks {})",
            block["candidate"].as_str().unwrap_or_default(),
            block["receipt_sha256"].as_str().unwrap_or_default(),
            block["envelope_commit"].as_str().unwrap_or_default(),
            block["included_tasks"]
                .as_array()
                .map(|ids| ids
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ),
        None => "snapshot".to_owned(),
    };
    let body = format!(
        "Qualified tree {tree} (commit {commit}) on {host}: {}.\nSource: {origin}.\nOwed: {}.",
        ALL_QUALIFICATION_GATES.join(", "),
        if owed.is_empty() {
            "none".to_owned()
        } else {
            owed.join(", ")
        }
    );
    git(&root, &["commit", "-q", "-m", &subject, "-m", &body])?;
    let record_commit = git(&root, &["rev-parse", "HEAD"])?;
    verify_tag(&root, &tag, &record_commit)
        .map_err(|e| format!("the record commit does not verify: {e}"))?;
    let message = format!(
        "Susi core {version}\n\nQualified tree {tree}; record {path} sha256 {}.",
        sha256_hex(text.as_bytes())
    );
    git(&root, &["tag", "-a", &tag, "-m", &message, &record_commit])?;
    Ok(format!(
        "Qualified {tag} from {source}: record commit {record_commit}, tag created locally.\nPublish with: git push origin {tag}"
    ))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn test_counts_sum_every_result_line_and_ignore_other_output() {
        let log = "running 2 tests\n\
                   test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.1s\n\
                   error: something else\n\
                   test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 3 filtered out; finished in 1s\n\
                   Doc-tests susi_core\n\
                   test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.2s\n";
        let counts = super::test_counts(log);
        assert_eq!(
            counts,
            serde_json::json!({"binaries": 3, "passed": 10, "failed": 2, "ignored": 1})
        );
    }

    #[test]
    fn parallel_test_concurrency_clamps_lanes_and_honors_jobs_override() {
        assert_eq!(super::parallel_test_concurrency(None, 2), (2, 4, 1));
        assert_eq!(super::parallel_test_concurrency(None, 8), (8, 16, 1));
        assert_eq!(super::parallel_test_concurrency(None, 32), (12, 32, 2));
        assert_eq!(super::parallel_test_concurrency(Some("6"), 2), (6, 12, 1));
        assert_eq!(super::parallel_test_concurrency(Some("24"), 4), (12, 24, 2));
    }

    #[test]
    fn resolve_target_dir_handles_default_relative_and_absolute_overrides() {
        let root = Path::new("/tmp/susi-root");
        assert_eq!(
            super::resolve_target_dir_from(root, None),
            root.join("target")
        );
        assert_eq!(
            super::resolve_target_dir_from(root, Some(OsStr::new(""))),
            root.join("target")
        );
        assert_eq!(
            super::resolve_target_dir_from(root, Some(OsStr::new("custom-target"))),
            root.join("custom-target")
        );
        let abs = std::env::temp_dir().join("susi-abs-target");
        assert_eq!(
            super::resolve_target_dir_from(root, Some(abs.as_os_str())),
            abs
        );
    }

    #[test]
    fn summarize_startup_and_conformance_json_enforce_pass_and_2000ms_budget() {
        assert_eq!(
            super::ALL_QUALIFICATION_GATES,
            [
                "format",
                "check",
                "clippy",
                "licenses",
                "tests",
                "build",
                "smoke",
                "startup",
                "conformance"
            ]
        );

        let startup_ok = br#"{"schema":"susi-startup-gate-v2","pass":true,"floor":{"median_ms":2.13},"cases":[]}"#;
        let (summary, schema) = super::summarize_startup_gate_json(startup_ok).unwrap();
        assert_eq!(schema, "susi-startup-gate-v2");
        assert_eq!(summary["pass"], true);
        assert_eq!(summary["floor_ms"], 2.13);

        let startup_fail =
            br#"{"schema":"susi-startup-gate-v2","pass":false,"floor":{"median_ms":9.5}}"#;
        assert!(super::summarize_startup_gate_json(startup_fail)
            .unwrap_err()
            .contains("pass != true"));

        let conf_ok = br#"{"conformant":true,"checks":[{"name":"health","ok":true,"ms":1},{"name":"udp.discovery","ok":true,"ms":513}]}"#;
        let (conf_summary, max_ms, checks) =
            super::summarize_conformance_json(conf_ok, 2_000).unwrap();
        assert_eq!(max_ms, 513);
        assert_eq!(checks, 2);
        assert_eq!(
            conf_summary,
            serde_json::json!({
                "conformant": true,
                "budget_ms": 2000,
                "max_check_ms": 513,
                "checks": 2
            })
        );

        let conf_over_budget =
            br#"{"conformant":true,"checks":[{"name":"mcp.stdio","ok":true,"ms":2001}]}"#;
        assert!(super::summarize_conformance_json(conf_over_budget, 2_000)
            .unwrap_err()
            .contains("exceeds 2000 ms budget"));

        let conf_not_ok =
            br#"{"conformant":false,"checks":[{"name":"mcp.stdio","ok":false,"ms":42}]}"#;
        assert!(super::summarize_conformance_json(conf_not_ok, 2_000)
            .unwrap_err()
            .contains("conformant != true"));
    }
}
