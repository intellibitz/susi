//! OS-neutral local Susi development channel manager (`D-OS-6`).
//!
//! Ports `scripts/susi-local.ps1` and `scripts/test-local-channel.ps1` to
//! portable Rust so `build`, `run`, `rollback`, `status`, and
//! `verify-candidate` behave identically on Windows, Linux, and macOS.

#![forbid(unsafe_code)]

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PRODUCTION_REFUSAL: &str = "Production requires the native release workflow. Use cargo local-release status, qualify, promote, rollback or run.";
const PROOF_GOAL: &str = "Read proof.txt and return its exact contents.";

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "susi.exe"
    } else {
        "susi"
    }
}

fn default_local_root() -> Result<PathBuf> {
    if cfg!(windows) {
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("SusiLocal"));
        }
        if let Some(home) = env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(home).join("AppData/Local/SusiLocal"));
        }
        return Err("missing LOCALAPPDATA or USERPROFILE".into());
    }
    if cfg!(target_os = "macos") {
        let home = env::var_os("HOME").ok_or("missing HOME")?;
        return Ok(PathBuf::from(home).join("Library/Application Support/SusiLocal"));
    }
    if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("SusiLocal"));
        }
    }
    let home = env::var_os("HOME").ok_or("missing HOME")?;
    Ok(PathBuf::from(home).join(".local/share/SusiLocal"))
}

fn validate_channel(channel: &str) -> Result<()> {
    match channel {
        "dev" => Ok(()),
        "production" => Err(PRODUCTION_REFUSAL.into()),
        other => {
            Err(format!("unsupported channel `{other}` (expected `dev` or `production`)").into())
        }
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn is_lower_hex_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn channel_state_path(channel_root: &Path) -> PathBuf {
    channel_root.join("channel.json")
}

fn read_state(state_path: &Path) -> Result<Option<Value>> {
    if !state_path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(state_path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn write_state(state_path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = state_path.with_file_name(format!(
        "{}.{}.new",
        state_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("channel.json"),
        std::process::id()
    ));
    let payload = serde_json::to_string_pretty(value)?;
    fs::write(&temp, format!("{payload}\n"))?;
    fs::rename(&temp, state_path)?;
    Ok(())
}

fn probe_version(binary: &Path) -> Result<String> {
    let out = Command::new(binary).arg("--version").output()?;
    if !out.status.success() {
        return Err("version probe exited non-zero".into());
    }
    let version = String::from_utf8(out.stdout)?.trim().to_string();
    if !version.starts_with("susi ") {
        return Err(format!("unexpected version output: {version}").into());
    }
    Ok(version)
}

fn set_executable_if_unix(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn stage_candidate_into_slot(
    channel_root: &Path,
    built: &Path,
) -> Result<(PathBuf, String, String)> {
    if !built.is_file() {
        return Err(format!("Expected native executable missing: {}", built.display()).into());
    }
    let digest = sha256_file(built)?;
    let slot = channel_root.join("builds").join(&digest);
    fs::create_dir_all(&slot)?;
    let candidate = slot.join(exe_name());
    if !candidate.is_file() {
        fs::copy(built, &candidate)?;
    }
    set_executable_if_unix(&candidate)?;
    if sha256_file(&candidate)? != digest {
        return Err("Local candidate checksum mismatch.".into());
    }
    let version = probe_version(&candidate)
        .map_err(|_| "Local candidate version check failed; current channel unchanged.")?;
    Ok((candidate, digest, version))
}

fn rollback_channel(channel_root: &Path, channel: &str) -> Result<Value> {
    validate_channel(channel)?;
    let state_path = channel_state_path(channel_root);
    let Some(mut state) = read_state(&state_path)? else {
        return Err("No previous local build is available.".into());
    };
    let prev_str = state
        .get("previous")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("No previous local build is available.")?
        .to_string();
    let prev_path = PathBuf::from(&prev_str);
    if !prev_path.is_file() {
        return Err("No previous local build is available.".into());
    }
    let digest = sha256_file(&prev_path)?;
    let expected = prev_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if !is_lower_hex_sha256(expected) || expected != digest {
        return Err("Previous local executable checksum changed; rollback refused.".into());
    }
    let version = probe_version(&prev_path)
        .map_err(|_| "Previous local executable version check failed; rollback refused.")?;
    let old_current = state.get("current").cloned().unwrap_or(Value::Null);
    if let Some(obj) = state.as_object_mut() {
        obj.insert("current".to_string(), Value::String(prev_str));
        obj.insert("previous".to_string(), old_current);
        obj.insert("sha256".to_string(), Value::String(digest));
        obj.insert("version".to_string(), Value::String(version));
    }
    write_state(&state_path, &state)?;
    println!("Local {channel} channel rolled back.");
    Ok(state)
}

fn configure_isolated_env(cmd: &mut Command, scratch: &Path) {
    for (key, _) in env::vars() {
        if key.ends_with("_API_KEY") || key.ends_with("_API_BASE") || key.ends_with("_BASE_URL") {
            cmd.env_remove(&key);
        }
    }
    for key in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
    ] {
        cmd.env(key, scratch);
    }
    cmd.env("SUSI_HOME", scratch.join("home"))
        .env("SUSI_PORT_OFFSET", "400")
        .env("SUSI_OFFLINE", "1")
        .env("RUST_LOG", "warn")
        .env_remove("SUSI_VERBOSE");
}

fn stop_isolated_daemon(binary: &Path, scratch: &Path) {
    let mut stop = Command::new(binary);
    stop.arg("stop")
        .current_dir(scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_env(&mut stop, scratch);
    let _ = stop.status();
}

fn verify_candidate(binary: &Path) -> Result<PathBuf> {
    let binary = fs::canonicalize(binary)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let scratch = env::temp_dir().join(format!("susi-local-check-{}-{nanos}", std::process::id()));
    fs::create_dir_all(scratch.join("home"))?;
    let nonce = format!("local-proof-{}-{nanos}", std::process::id());
    fs::write(scratch.join("proof.txt"), &nonce)?;
    fs::write(scratch.join("home/privacy_mode"), "local_only")?;

    let mut cmd = Command::new(&binary);
    cmd.current_dir(&scratch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_isolated_env(&mut cmd, &scratch);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Candidate could not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{PROOF_GOAL}");
    }
    let output = child.wait_with_output();
    stop_isolated_daemon(&binary, &scratch);

    let output = output?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    fs::write(scratch.join("stdout.txt"), &stdout)?;
    fs::write(scratch.join("stderr.txt"), &stderr)?;

    if !output.status.success() || !stdout.contains(&nonce) {
        return Err(format!(
            "Local first-goal verification failed. Evidence retained: {}",
            scratch.display()
        )
        .into());
    }
    let trace_path = scratch.join(".susi/last_mission_trace.json");
    let trace_raw = fs::read_to_string(&trace_path).map_err(|_| {
        format!(
            "No successful governed result recorded. Evidence retained: {}",
            scratch.display()
        )
    })?;
    let trace: Value = serde_json::from_str(&trace_raw)?;
    let goal_ok = trace.get("goal").and_then(Value::as_str) == Some(PROOF_GOAL);
    let status_ok = matches!(
        trace.get("status").and_then(Value::as_str),
        Some("SUCCESS" | "COMPLETE")
    );
    let answer_ok = trace
        .get("final_answer")
        .and_then(Value::as_str)
        .is_some_and(|ans| ans.contains(&nonce));
    if !goal_ok || !status_ok || !answer_ok {
        return Err(format!(
            "No successful governed result recorded. Evidence retained: {}",
            scratch.display()
        )
        .into());
    }
    println!(
        "Local installed candidate completed a fresh file goal. Evidence: {}",
        scratch.display()
    );
    Ok(scratch)
}

fn repo_root() -> Result<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "missing repository root".into())
}

fn build_channel(channel_root: &Path, channel: &str) -> Result<Value> {
    validate_channel(channel)?;
    let start = Instant::now();
    let repo = repo_root()?;
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { repo.join(p) })
        .unwrap_or_else(|| repo.join("target"));

    let status = Command::new("cargo")
        .current_dir(&repo)
        .env("CARGO_PROFILE_DEV_OPT_LEVEL", "0")
        .env("CARGO_PROFILE_DEV_LTO", "false")
        .env("CARGO_PROFILE_DEV_INCREMENTAL", "true")
        .env_remove("RUSTC_WRAPPER")
        .args([
            "--config",
            "build.rustc-wrapper=''",
            "build",
            "--locked",
            "--bin",
            "susi",
            "--profile",
            "dev",
            "--timings",
        ])
        .status()?;
    if !status.success() {
        return Err(format!("Local build failed ({status}); current channel unchanged.").into());
    }

    let built = target_dir.join("debug").join(exe_name());
    let (candidate, digest, version) = stage_candidate_into_slot(channel_root, &built)?;
    verify_candidate(&candidate)?;

    let state_path = channel_state_path(channel_root);
    let existing = read_state(&state_path)?;
    let candidate_str = candidate.to_string_lossy().into_owned();
    let previous = match existing.as_ref() {
        Some(prev_state) => {
            let old_current = prev_state.get("current").and_then(Value::as_str);
            if old_current.is_some_and(|c| c != candidate_str) {
                prev_state.get("current").cloned().unwrap_or(Value::Null)
            } else {
                prev_state.get("previous").cloned().unwrap_or(Value::Null)
            }
        }
        None => Value::Null,
    };

    let elapsed = (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let built_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let new_state = json!({
        "channel": channel,
        "current": candidate_str,
        "previous": previous,
        "sha256": digest,
        "version": version,
        "builtUtc": built_unix.to_string(),
        "buildSeconds": elapsed,
    });
    write_state(&state_path, &new_state)?;
    println!("Local {channel} channel ready: {version} ({elapsed:.1} seconds).");
    Ok(new_state)
}

fn run_channel(channel_root: &Path, channel: &str, susi_args: &[String]) -> Result<()> {
    validate_channel(channel)?;
    let state_path = channel_state_path(channel_root);
    let Some(state) = read_state(&state_path)? else {
        return Err(format!("Build the local {channel} channel first.").into());
    };
    let current = state
        .get("current")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("Build the local {channel} channel first."))?;
    let expected_sha = state
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sha256_file(&current)? != expected_sha {
        return Err(
            "Activated local executable checksum changed; rebuild or roll back before running."
                .into(),
        );
    }
    let status = Command::new(&current)
        .args(susi_args)
        .env("SUSI_HOME", channel_root.join("home"))
        .env("SUSI_PORT_OFFSET", "200")
        .status()?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        return Err(format!("Local {channel} Susi exited with code {code}.").into());
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Status,
    Rollback,
    Build,
    Run(Vec<String>),
    VerifyCandidate(Option<PathBuf>),
}

fn parse_cli(args: &[String]) -> Result<(Action, String, PathBuf)> {
    let mut channel = "dev".to_string();
    let mut root: Option<PathBuf> = None;
    let mut action: Option<Action> = None;
    let mut idx = 0;

    while idx < args.len() {
        match args[idx].as_str() {
            "--channel" | "-Channel" => {
                idx += 1;
                let val = args.get(idx).ok_or("missing value for --channel")?;
                channel = val.to_lowercase();
            }
            "--root" | "-Root" => {
                idx += 1;
                let val = args.get(idx).ok_or("missing value for --root")?;
                root = Some(PathBuf::from(val));
            }
            "--action" | "-Action" => {
                idx += 1;
                let val = args.get(idx).ok_or("missing value for --action")?;
                action = Some(match val.to_lowercase().as_str() {
                    "status" => Action::Status,
                    "rollback" => Action::Rollback,
                    "build" => Action::Build,
                    "run" => Action::Run(args[idx + 1..].to_vec()),
                    other => return Err(format!("unknown action `{other}`").into()),
                });
                if matches!(action, Some(Action::Run(_))) {
                    break;
                }
            }
            "status" if action.is_none() => action = Some(Action::Status),
            "rollback" if action.is_none() => action = Some(Action::Rollback),
            "build" if action.is_none() => action = Some(Action::Build),
            "run" if action.is_none() => {
                let rest = if args.get(idx + 1).map(String::as_str) == Some("--") {
                    args[idx + 2..].to_vec()
                } else {
                    args[idx + 1..].to_vec()
                };
                action = Some(Action::Run(rest));
                break;
            }
            "verify-candidate" if action.is_none() => {
                let candidate = args.get(idx + 1).map(PathBuf::from);
                if candidate.is_some() {
                    idx += 1;
                }
                action = Some(Action::VerifyCandidate(candidate));
            }
            other => {
                return Err(format!(
                    "unexpected argument `{other}` (usage: local-channel [status|build|rollback|run|verify-candidate] [--channel dev] [--root <dir>])"
                )
                .into());
            }
        }
        idx += 1;
    }

    let resolved_root = match root {
        Some(r) => r,
        None => default_local_root()?,
    };
    Ok((action.unwrap_or(Action::Build), channel, resolved_root))
}

fn run() -> Result<()> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let (action, channel, root) = parse_cli(&raw_args)?;
    validate_channel(&channel)?;
    let channel_root = root.join(&channel);

    match action {
        Action::Status => {
            let state = read_state(&channel_state_path(&channel_root))?;
            match state {
                Some(val) => println!("{}", serde_json::to_string_pretty(&val)?),
                None => println!("null"),
            }
            Ok(())
        }
        Action::Rollback => {
            rollback_channel(&channel_root, &channel)?;
            Ok(())
        }
        Action::Build => {
            build_channel(&channel_root, &channel)?;
            Ok(())
        }
        Action::Run(susi_args) => run_channel(&channel_root, &channel, &susi_args),
        Action::VerifyCandidate(binary) => {
            let bin = match binary {
                Some(b) => b,
                None => repo_root()?.join("target/debug").join(exe_name()),
            };
            verify_candidate(&bin)?;
            Ok(())
        }
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mock_susi_binary(path: &Path, version: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        #[cfg(windows)]
        {
            // On Windows, create a tiny batch/cmd or compile a minimal rust binary so
            // `Command::new(path).arg("--version")` succeeds and prints `susi <version>`.
            let src_path = path.with_extension("rs");
            fs::write(
                &src_path,
                format!("fn main() {{ println!(\"susi {version}\"); }}\n"),
            )?;
            let status = Command::new("rustc")
                .arg(&src_path)
                .arg("-o")
                .arg(path)
                .status()?;
            let _ = fs::remove_file(&src_path);
            assert!(status.success(), "rustc failed to compile mock binary");
        }
        #[cfg(not(windows))]
        {
            fs::write(path, format!("#!/bin/sh\necho \"susi {version}\"\n"))?;
            set_executable_if_unix(path)?;
        }
        Ok(())
    }

    #[test]
    fn production_channel_is_refused() -> Result<()> {
        let Err(err) = validate_channel("production") else {
            return Err("expected production channel to be refused".into());
        };
        assert_eq!(err.to_string(), PRODUCTION_REFUSAL);
        assert!(validate_channel("dev").is_ok());
        Ok(())
    }

    #[test]
    fn state_atomic_round_trip() -> Result<()> {
        let dir = env::temp_dir().join(format!("susi-local-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let state_path = channel_state_path(&dir.join("dev"));
        assert!(read_state(&state_path)?.is_none());

        let expected = json!({
            "channel": "dev",
            "current": "/tmp/susi-a",
            "previous": null,
            "sha256": "a".repeat(64),
            "version": "susi 0.21.47",
        });
        write_state(&state_path, &expected)?;
        let loaded = read_state(&state_path)?.ok_or("state must exist")?;
        assert_eq!(loaded, expected);

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn stage_and_rollback_swaps_verified_slots_and_refuses_tampering() -> Result<()> {
        let dir = env::temp_dir().join(format!("susi-local-rollback-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let channel_root = dir.join("dev");
        fs::create_dir_all(&channel_root)?;

        let built_v1 = dir.join(format!("built-v1-{}", exe_name()));
        let built_v2 = dir.join(format!("built-v2-{}", exe_name()));
        write_mock_susi_binary(&built_v1, "0.21.46")?;
        write_mock_susi_binary(&built_v2, "0.21.47")?;

        let (cand_v1, sha_v1, ver_v1) = stage_candidate_into_slot(&channel_root, &built_v1)?;
        let (cand_v2, sha_v2, ver_v2) = stage_candidate_into_slot(&channel_root, &built_v2)?;
        assert_ne!(sha_v1, sha_v2);
        assert_eq!(ver_v1, "susi 0.21.46");
        assert_eq!(ver_v2, "susi 0.21.47");

        let state_path = channel_state_path(&channel_root);
        let initial_state = json!({
            "channel": "dev",
            "current": cand_v2.to_string_lossy(),
            "previous": cand_v1.to_string_lossy(),
            "sha256": sha_v2,
            "version": ver_v2,
        });
        write_state(&state_path, &initial_state)?;

        let rolled = rollback_channel(&channel_root, "dev")?;
        assert_eq!(
            rolled.get("current").and_then(Value::as_str),
            Some(cand_v1.to_string_lossy().as_ref())
        );
        assert_eq!(
            rolled.get("previous").and_then(Value::as_str),
            Some(cand_v2.to_string_lossy().as_ref())
        );
        assert_eq!(
            rolled.get("sha256").and_then(Value::as_str),
            Some(sha_v1.as_str())
        );
        assert_eq!(
            rolled.get("version").and_then(Value::as_str),
            Some("susi 0.21.46")
        );

        // Tamper with the previous binary (`cand_v2`) and verify rollback is refused.
        let mut bytes = fs::read(&cand_v2)?;
        bytes.push(0x00);
        fs::write(&cand_v2, bytes)?;
        let Err(err) = rollback_channel(&channel_root, "dev") else {
            return Err("expected tampered rollback to fail".into());
        };
        let err = err.to_string();
        assert!(
            err.contains("Previous local executable checksum changed; rollback refused."),
            "unexpected error: {err}"
        );

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
