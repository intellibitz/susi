//! Native exact-tree release qualification (`docs/releases.md`).
//!
//! `cargo run -p xtask --bin release-qualify -- vX.Y.Z [--jobs N]`, from a clean
//! checkout of the release tree whose package version is X.Y.Z. It runs every
//! gate in `REQUIRED_GATES` on that exact committed tree, keeps each gate's log
//! under `target/release-qualification/<tag>/`, and only when all of them pass
//! writes `releases/qualification/<tag>.json`, commits that one file, checks
//! the commit with the same verifier push-policy uses, and creates the local
//! annotated tag. Pushing the tag (`git push origin vX.Y.Z`) runs push-policy,
//! which admits it only with that record. A failed gate stops the run: nothing
//! is written, committed or tagged.
#![forbid(unsafe_code)]
#[cfg(windows)]
extern crate susi_windows_runtime;
#[path = "../release_qualification.rs"]
mod release_qualification;

use release_qualification::{
    package_version, record_path, release_version, sha256_hex, toolchain_channel, verify_tag,
    REQUIRED_GATES, SCHEMA,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The binaries a Susi core release ships, as built by the `build` gate.
const ASSETS: [&str; 2] = ["susi", "susi-harness"];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
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

/// Each shipped binary answers `--version` with the release version, from a
/// private home so no operator state is read or written.
fn smoke(root: &Path, logs: &Path, version: &str) -> Result<(Value, Vec<Value>), String> {
    let home = logs.join("smoke-home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let mut assets = Vec::new();
    let mut argv = Vec::new();
    let started = Instant::now();
    for name in ASSETS {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let path = root.join("target").join("dist").join(&file);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let out = Command::new(&path)
            .arg("--version")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("SUSI_HOME", home.join(".susi"))
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .current_dir(&home)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {file}: {e}"))?;
        let answer = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || !answer.contains(version) {
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
    let gate = json!({
        "name": "smoke",
        "argv": argv,
        "env": ["HOME=<private>", "SUSI_HOME=<private>", "SUSI_NO_REGISTRY_ENV=1"],
        "exit_code": 0,
        "duration_s": started.elapsed().as_secs(),
    });
    Ok((gate, assets))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: release-qualify vX.Y.Z [--jobs N]";
    let tag = args.first().ok_or(usage)?.clone();
    let jobs = match args.get(1).map(String::as_str) {
        None => None,
        Some("--jobs") => Some(args.get(2).ok_or(usage)?.as_str()),
        Some(_) => return Err(usage.into()),
    };
    let version = release_version(&tag).ok_or(usage)?.to_owned();
    let root = PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"])?);
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
    let logs = root.join("target").join("release-qualification").join(&tag);
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
    let started = now();
    let mut records = Vec::new();
    for gate in gates(jobs) {
        records.push(run_gate(&root, &logs, &gate)?);
    }
    let (smoke_gate, assets) = smoke(&root, &logs, &version)?;
    records.push(smoke_gate);
    let names: Vec<&str> = records.iter().filter_map(|g| g["name"].as_str()).collect();
    if REQUIRED_GATES
        .iter()
        .any(|required| !names.contains(required))
    {
        return Err("a required gate did not run".into());
    }
    let owed: Vec<&str> = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
        .into_iter()
        .filter(|target| *target != host)
        .collect();
    let record = json!({
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
    });
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())? + "\n";
    let file = root.join(&path);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, &text).map_err(|e| e.to_string())?;
    git(&root, &["add", "--", &path])?;
    let subject = format!("chore: release {tag} qualification record");
    let body = format!(
        "Qualified tree {tree} (commit {commit}) on {host}: {}.\nOwed: {}.",
        REQUIRED_GATES.join(", "),
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
    println!(
        "Qualified {tag}: record commit {record_commit}, tag created locally.\nPublish with: git push origin {tag}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_counts_sum_every_result_line_and_ignore_other_output() {
        let log = "running 2 tests\n\
                   test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.1s\n\
                   error: something else\n\
                   test result: FAILED. 5 passed; 2 failed; 0 ignored; 0 measured; 3 filtered out; finished in 1s\n";
        let counts = super::test_counts(log);
        assert_eq!(
            counts,
            serde_json::json!({"binaries": 2, "passed": 7, "failed": 2, "ignored": 1})
        );
    }
}
