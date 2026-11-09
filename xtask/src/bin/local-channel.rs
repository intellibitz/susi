//! OS-neutral local Susi development channel manager (`D-OS-6`).
//!
//! Ports `scripts/susi-local.ps1` and `scripts/test-local-channel.ps1` to
//! portable Rust so `build`, `run`, `rollback`, `status`, and
//! `verify-candidate` behave identically on Windows, Linux, and macOS.
//!
//! `deliver` is the Step 1 change-to-live loop for the dev channel: from a
//! committed change, it tests the packages that own the changed files, builds
//! the dev executable, runs the governed first-goal check, fast-forwards the
//! dev live lineage ref, promotes, observes the promoted executable and can
//! prove rollback, recording every phase's wall clock. It never touches the
//! production channel; production keeps `local-release qualify`.

#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../affected.rs"]
mod affected;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PRODUCTION_REFUSAL: &str = "Production requires the native release workflow. Use cargo local-release status, qualify, promote, rollback or run.";
const PROOF_GOAL: &str = "Read proof.txt and return its exact contents.";
/// The dev channel's live lineage. Shared by every worktree of the repository,
/// so deliveries from different agents form one fast-forward-only history.
const DEFAULT_LIVE_REF: &str = "refs/susi/dev-live";
/// Step 1 (Mandate 62): a genuine change reaches the verified live executable
/// in under 60 seconds.
const LOOP_BUDGET_MS: u128 = 60_000;

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "susi.exe"
    } else {
        "susi"
    }
}

fn resolve_local_root_for(
    os: &str,
    localappdata: Option<&Path>,
    userprofile: Option<&Path>,
    home: Option<&Path>,
    xdg_data: Option<&Path>,
) -> Result<PathBuf> {
    match os {
        "windows" => {
            if let Some(local) = localappdata.filter(|p| !p.as_os_str().is_empty()) {
                return Ok(local.join("SusiLocal"));
            }
            if let Some(profile) = userprofile.filter(|p| !p.as_os_str().is_empty()) {
                return Ok(profile.join("AppData/Local/SusiLocal"));
            }
            Err("missing LOCALAPPDATA or USERPROFILE".into())
        }
        "macos" => {
            let home = home
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or("missing HOME")?;
            Ok(home.join("Library/Application Support/SusiLocal"))
        }
        _ => {
            if let Some(xdg) = xdg_data.filter(|p| !p.as_os_str().is_empty()) {
                return Ok(xdg.join("SusiLocal"));
            }
            let home = home
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or("missing HOME")?;
            Ok(home.join(".local/share/SusiLocal"))
        }
    }
}

fn default_local_root() -> Result<PathBuf> {
    let localappdata = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let userprofile = env::var_os("USERPROFILE").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    let xdg_data = env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    resolve_local_root_for(
        env::consts::OS,
        localappdata.as_deref(),
        userprofile.as_deref(),
        home.as_deref(),
        xdg_data.as_deref(),
    )
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
        .env("SUSI_NO_REGISTRY_ENV", "1")
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

/// Wall clock of the governed first-goal check, from spawning the candidate.
#[derive(Debug, Clone, Copy)]
struct GoalTiming {
    /// First byte the candidate wrote to stdout (`None` if it wrote nothing).
    first_output: Option<Duration>,
    /// The candidate process exited.
    completion: Duration,
}

/// Drain `reader` on its own thread, noting when its first byte arrived.
fn drain_timed<R: Read + Send + 'static>(
    mut reader: R,
    start: Instant,
) -> std::thread::JoinHandle<(Vec<u8>, Option<Duration>)> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut first = None;
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if first.is_none() {
                        first = Some(start.elapsed());
                    }
                    bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
                }
            }
        }
        (bytes, first)
    })
}

fn verify_candidate(binary: &Path) -> Result<PathBuf> {
    verify_candidate_timed(binary).map(|(scratch, _)| scratch)
}

fn verify_candidate_timed(binary: &Path) -> Result<(PathBuf, GoalTiming)> {
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

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Candidate could not start: {e}"))?;
    let stdout_reader = child.stdout.take().map(|out| drain_timed(out, start));
    let stderr_reader = child.stderr.take().map(|err| drain_timed(err, start));
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{PROOF_GOAL}");
    }
    let status = child.wait();
    let completion = start.elapsed();
    let (stdout_bytes, first_output) = match stdout_reader {
        Some(handle) => handle
            .join()
            .map_err(|_| "candidate stdout reader panicked")?,
        None => (Vec::new(), None),
    };
    let stderr_bytes = match stderr_reader {
        Some(handle) => {
            handle
                .join()
                .map_err(|_| "candidate stderr reader panicked")?
                .0
        }
        None => Vec::new(),
    };
    stop_isolated_daemon(&binary, &scratch);

    let status = status?;
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    fs::write(scratch.join("stdout.txt"), &stdout)?;
    fs::write(scratch.join("stderr.txt"), &stderr)?;

    if !status.success() || !stdout.contains(&nonce) {
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
    Ok((
        scratch,
        GoalTiming {
            first_output,
            completion,
        },
    ))
}

fn repo_root() -> Result<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "missing repository root".into())
}

/// `cargo` with the dev channel's build settings. The delivery test run and the
/// executable build share them, so both reuse one set of dependency artifacts.
fn dev_cargo(repo: &Path) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(repo)
        .env("CARGO_PROFILE_DEV_OPT_LEVEL", "0")
        .env("CARGO_PROFILE_DEV_LTO", "false")
        .env("CARGO_PROFILE_DEV_INCREMENTAL", "true")
        .env_remove("RUSTC_WRAPPER")
        .args(["--config", "build.rustc-wrapper=''"]);
    cmd
}

fn build_dev_executable(repo: &Path) -> Result<PathBuf> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|p| if p.is_absolute() { p } else { repo.join(p) })
        .unwrap_or_else(|| repo.join("target"));
    let status = dev_cargo(repo)
        .args([
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
    Ok(target_dir.join("debug").join(exe_name()))
}

/// Atomically make a staged, verified candidate the channel's current build.
/// A candidate staged in its digest-named slot and version-checked.
struct Staged<'a> {
    candidate: &'a Path,
    digest: &'a str,
    version: &'a str,
}

fn promote_candidate(
    channel_root: &Path,
    channel: &str,
    staged: &Staged<'_>,
    build_seconds: f64,
    commit: Option<&str>,
) -> Result<Value> {
    let Staged {
        candidate,
        digest,
        version,
    } = *staged;
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
    let built_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut new_state = json!({
        "channel": channel,
        "current": candidate_str,
        "previous": previous,
        "sha256": digest,
        "version": version,
        "builtUtc": built_unix.to_string(),
        "buildSeconds": build_seconds,
    });
    if let (Some(commit), Some(obj)) = (commit, new_state.as_object_mut()) {
        obj.insert("commit".to_string(), Value::String(commit.to_string()));
    }
    write_state(&state_path, &new_state)?;
    Ok(new_state)
}

fn build_channel(channel_root: &Path, channel: &str) -> Result<Value> {
    validate_channel(channel)?;
    let start = Instant::now();
    let repo = repo_root()?;
    let built = build_dev_executable(&repo)?;
    let (candidate, digest, version) = stage_candidate_into_slot(channel_root, &built)?;
    verify_candidate(&candidate)?;
    let elapsed = (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let new_state = promote_candidate(
        channel_root,
        channel,
        &Staged {
            candidate: &candidate,
            digest: &digest,
            version: &version,
        },
        elapsed,
        None,
    )?;
    println!("Local {channel} channel ready: {version} ({elapsed:.1} seconds).");
    Ok(new_state)
}

/// Options for [`deliver`].
#[derive(Debug, Default, PartialEq, Eq)]
struct DeliverOptions {
    /// After observing the change, roll back, observe the previous build, and
    /// re-promote the delivered build.
    prove_rollback: bool,
    /// Lineage ref to fast-forward (default [`DEFAULT_LIVE_REF`]).
    live_ref: Option<String>,
    /// `susi` arguments that observe the changed behavior (default `--version`).
    observe: Vec<String>,
}

/// Successive wall-clock phases of one delivery.
struct PhaseClock {
    start: Instant,
    last: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl PhaseClock {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last: now,
            phases: Vec::new(),
        }
    }

    fn mark(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.phases.push((phase, now.duration_since(self.last)));
        self.last = now;
    }

    fn total(&self) -> Duration {
        self.last.duration_since(self.start)
    }
}

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10_000.0).round() / 10.0
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").current_dir(repo).args(args).output()?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_succeeds(repo: &Path, args: &[&str]) -> Result<bool> {
    Ok(Command::new("git")
        .current_dir(repo)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

/// A workspace member and the unit-test targets it has.
#[derive(Debug, Clone)]
struct PackageTargets {
    member: affected::Member,
    lib: bool,
    bins: bool,
}

/// Workspace members from `cargo metadata --no-deps` (offline, locked).
fn workspace_members(repo: &Path) -> Result<Vec<PackageTargets>> {
    let out = Command::new("cargo")
        .current_dir(repo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--locked",
            "--format-version",
            "1",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let metadata: Value = serde_json::from_slice(&out.stdout)?;
    let workspace_root = PathBuf::from(
        metadata["workspace_root"]
            .as_str()
            .ok_or("cargo metadata has no workspace_root")?,
    );
    let mut members = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
    {
        let name = package["name"]
            .as_str()
            .ok_or("package without name")?
            .to_string();
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .ok_or("package without manifest")?,
        );
        let dir = manifest
            .parent()
            .and_then(|dir| dir.strip_prefix(&workspace_root).ok())
            .ok_or("member outside the workspace root")?
            .to_string_lossy()
            .replace('\\', "/");
        let has_kind = |wanted: &[&str]| {
            package["targets"].as_array().is_some_and(|targets| {
                targets.iter().any(|target| {
                    target["kind"].as_array().is_some_and(|kinds| {
                        kinds
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|kind| wanted.contains(&kind))
                    })
                })
            })
        };
        members.push(PackageTargets {
            lib: has_kind(&["lib", "rlib", "dylib", "proc-macro"]),
            bins: has_kind(&["bin"]),
            member: affected::Member { name, dir },
        });
    }
    Ok(members)
}

/// The packages owning `changed`. A path that can change every package's
/// build (lockfile, toolchain, Cargo configuration, vendored code) is refused:
/// its verification is the full gate, which does not fit this loop.
fn owning_packages(members: &[PackageTargets], changed: &[String]) -> Result<Vec<PackageTargets>> {
    if let Some(path) = changed.iter().find(|path| affected::global(path)) {
        return Err(format!(
            "`{path}` can change every package; deliver it through the full gate (cargo local-workflow ship)."
        )
        .into());
    }
    let plain: Vec<affected::Member> = members.iter().map(|p| p.member.clone()).collect();
    let names: BTreeSet<String> = changed
        .iter()
        .filter_map(|path| affected::owner(&plain, path))
        .map(|member| member.name.clone())
        .collect();
    Ok(members
        .iter()
        .filter(|p| names.contains(&p.member.name))
        .cloned()
        .collect())
}

/// `cargo test` arguments for a package's own unit tests, or `None` when it
/// has neither a library nor a binary target.
fn unit_test_args(package: &PackageTargets) -> Option<Vec<String>> {
    if !package.lib && !package.bins {
        return None;
    }
    let mut args: Vec<String> = ["test", "--locked", "--offline", "-q", "-p"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.push(package.member.name.clone());
    if package.lib {
        args.push("--lib".to_string());
    }
    if package.bins {
        args.push("--bins".to_string());
    }
    Some(args)
}

/// Run each owning package's unit tests with credentials, Susi settings and
/// user homes removed, as the integration gate does.
fn run_owner_tests(repo: &Path, scratch: &Path, owners: &[PackageTargets]) -> Result<Vec<Value>> {
    const SECRET_PARTS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "API_KEY",
        "ACCESS_KEY",
        "API_BASE",
        "BASE_URL",
        "API_URL",
        "CUSTOM_HEADERS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ];
    let user_home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or("missing USERPROFILE or HOME")?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".rustup"));
    let mut records = Vec::new();
    for package in owners {
        let Some(args) = unit_test_args(package) else {
            records.push(json!({
                "package": package.member.name,
                "skipped": "no library or binary target",
            }));
            continue;
        };
        let home = scratch.join(&package.member.name);
        fs::create_dir_all(&home)?;
        let mut cmd = dev_cargo(repo);
        for (key, _) in env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("SUSI_") || SECRET_PARTS.iter().any(|part| name.contains(part)) {
                cmd.env_remove(key);
            }
        }
        let started = Instant::now();
        let status = cmd
            .args(&args)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("SUSI_HOME", home.join("susi"))
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("APPDATA", home.join("appdata"))
            .env("LOCALAPPDATA", home.join("localappdata"))
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .status()?;
        if !status.success() {
            return Err(format!(
                "Unit tests of `{}` failed ({status}); nothing was promoted.",
                package.member.name
            )
            .into());
        }
        records.push(json!({
            "package": package.member.name,
            "args": args,
            "ms": millis(started.elapsed()),
        }));
    }
    Ok(records)
}

/// The channel's current executable, after re-checking its recorded digest.
fn current_checked(channel_root: &Path, channel: &str) -> Result<PathBuf> {
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
    Ok(current)
}

/// Run the channel's current executable the way `run` does and capture what
/// it says, so a delivery records the behavior it made live.
fn observe_channel(channel_root: &Path, channel: &str, susi_args: &[String]) -> Result<Value> {
    let current = current_checked(channel_root, channel)?;
    let started = Instant::now();
    let out = Command::new(&current)
        .args(susi_args)
        .env("SUSI_HOME", channel_root.join("home"))
        .env("SUSI_PORT_OFFSET", "200")
        .stdin(Stdio::null())
        .output()?;
    let stdout: String = String::from_utf8_lossy(&out.stdout)
        .chars()
        .take(4096)
        .collect();
    Ok(json!({
        "executable": current.to_string_lossy(),
        "args": susi_args,
        "exit": out.status.code(),
        "stdout": stdout,
        "ms": millis(started.elapsed()),
    }))
}

/// Step 1's change-to-live loop for the dev channel. See the module docs.
fn deliver(channel_root: &Path, channel: &str, opts: &DeliverOptions) -> Result<Value> {
    validate_channel(channel)?;
    let mut clock = PhaseClock::new();
    let repo = repo_root()?;
    fs::create_dir_all(channel_root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(channel_root.join("deliver.lock"))?;
    lock.try_lock()
        .map_err(|_| "Another delivery holds this dev channel; deliver again when it finishes.")?;

    let status = git(
        &repo,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    if !status.is_empty() {
        return Err(format!(
            "deliver makes a commit live: commit or remove these changes first:\n{status}"
        )
        .into());
    }
    let head = git(&repo, &["rev-parse", "HEAD"])?;
    let live_ref = opts
        .live_ref
        .clone()
        .unwrap_or_else(|| DEFAULT_LIVE_REF.to_string());
    let live_spec = format!("{live_ref}^{{commit}}");
    let live = git_succeeds(&repo, &["rev-parse", "--verify", "--quiet", &live_spec])?
        .then(|| git(&repo, &["rev-parse", "--verify", "--quiet", &live_spec]))
        .transpose()?;
    let base = match &live {
        Some(live_commit) if *live_commit == head => {
            return Err(format!(
                "{head} is already live on the {channel} channel; commit a change first."
            )
            .into());
        }
        Some(live_commit) => {
            if !git_succeeds(&repo, &["merge-base", "--is-ancestor", live_commit, &head])? {
                return Err(format!(
                    "HEAD does not contain the live {channel} commit {live_commit} ({live_ref}); merge it first: git merge {live_ref}"
                )
                .into());
            }
            live_commit.clone()
        }
        None => git(&repo, &["merge-base", "HEAD", "refs/remotes/origin/main"])?,
    };
    let changed = affected::changed(&repo, &base, &head)?;
    if changed.is_empty() {
        return Err(format!("No file changed between {base} and HEAD.").into());
    }
    let owners = owning_packages(&workspace_members(&repo)?, &changed)?;
    let owner_names: Vec<String> = owners.iter().map(|p| p.member.name.clone()).collect();
    println!(
        "Delivering {} ({} changed files) to the {channel} channel; testing {}.",
        head.get(..12).unwrap_or(&head),
        changed.len(),
        owner_names.join(", ")
    );
    clock.mark("scope");

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let delivery_dir = channel_root
        .join("deliveries")
        .join(format!("{stamp}-{}", head.get(..12).unwrap_or(&head)));
    let tests = run_owner_tests(&repo, &delivery_dir.join("test-home"), &owners)?;
    clock.mark("test");

    let build_started = Instant::now();
    let built = build_dev_executable(&repo)?;
    let build_seconds = (build_started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    clock.mark("build");
    let (candidate, digest, version) = stage_candidate_into_slot(channel_root, &built)?;
    clock.mark("stage");
    let startup_started = Instant::now();
    probe_version(&candidate)?;
    let startup = startup_started.elapsed();
    clock.mark("startup");
    let (goal_evidence, goal) = verify_candidate_timed(&candidate)?;
    clock.mark("goal");

    let expected_old = live.clone().unwrap_or_else(|| "0".repeat(head.len()));
    git(
        &repo,
        &[
            "update-ref",
            "-m",
            "local-channel deliver",
            &live_ref,
            &head,
            &expected_old,
        ],
    )
    .map_err(|error| {
        format!("The {channel} live lineage moved during this delivery ({error}); merge {live_ref} and deliver again.")
    })?;
    clock.mark("merge");

    let promoted = match promote_candidate(
        channel_root,
        channel,
        &Staged {
            candidate: &candidate,
            digest: &digest,
            version: &version,
        },
        build_seconds,
        Some(&head),
    ) {
        Ok(state) => state,
        Err(error) => {
            let restored = match &live {
                Some(old) => git(&repo, &["update-ref", &live_ref, old, &head]),
                None => git(&repo, &["update-ref", "-d", &live_ref, &head]),
            };
            return Err(format!(
                "Promotion failed ({error}); live lineage restored: {}.",
                restored.is_ok()
            )
            .into());
        }
    };
    clock.mark("promote");

    let observe_args = if opts.observe.is_empty() {
        vec!["--version".to_string()]
    } else {
        opts.observe.clone()
    };
    let observed = observe_channel(channel_root, channel, &observe_args)?;
    clock.mark("observe");
    let change_to_live = clock.total();

    let rollback = if !opts.prove_rollback {
        Value::Null
    } else if promoted.get("previous").is_none_or(Value::is_null) {
        json!({"skipped": "no previous build to roll back to"})
    } else {
        let rollback_started = Instant::now();
        rollback_channel(channel_root, channel)?;
        let previous_observed = observe_channel(channel_root, channel, &observe_args)?;
        let rolled_back_ms = millis(rollback_started.elapsed());
        clock.mark("rollback");
        if sha256_file(&candidate)? != digest {
            return Err("Delivered executable changed during the rollback proof; the previous build stays current.".into());
        }
        write_state(&channel_state_path(channel_root), &promoted)?;
        let restored_observed = observe_channel(channel_root, channel, &observe_args)?;
        clock.mark("restore");
        json!({
            "rolled_back_ms": rolled_back_ms,
            "previous_observed": previous_observed,
            "restored_observed": restored_observed,
            "behavior_differs": previous_observed.get("stdout") != observed.get("stdout"),
        })
    };

    let phases_ms: serde_json::Map<String, Value> = clock
        .phases
        .iter()
        .map(|(name, duration)| ((*name).to_string(), json!(millis(*duration))))
        .collect();
    let within_budget = change_to_live.as_millis() < LOOP_BUDGET_MS;
    let record = json!({
        "schema": "susi-dev-delivery-v1",
        "channel": channel,
        "commit": head,
        "base": base,
        "live_ref": live_ref,
        "changed": changed,
        "tested_packages": owner_names,
        "tests": tests,
        "candidate": candidate.to_string_lossy(),
        "sha256": digest,
        "version": version,
        "startup_ms": millis(startup),
        "goal": {
            "first_output_ms": goal.first_output.map(millis),
            "completion_ms": millis(goal.completion),
            "evidence": goal_evidence.to_string_lossy(),
        },
        "observed": observed,
        "rollback": rollback,
        "phases_ms": phases_ms,
        "change_to_live_ms": millis(change_to_live),
        "budget_ms": LOOP_BUDGET_MS,
        "within_budget": within_budget,
        "covers": "dev channel: unit tests of the packages owning the changed files, the dev build, the governed first-goal check, lineage fast-forward, promotion and observation. Dependent packages' tests, the integration gate (cargo local-workflow ship) and production qualification (cargo local-release qualify) are not part of this loop.",
    });
    fs::create_dir_all(&delivery_dir)?;
    fs::write(
        delivery_dir.join("delivery.json"),
        format!("{}\n", serde_json::to_string_pretty(&record)?),
    )?;
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(channel_root.join("deliveries.jsonl"))?;
    writeln!(log, "{}", serde_json::to_string(&record)?)?;

    for (name, duration) in &clock.phases {
        println!("  {name:<9} {:>9.1} ms", millis(*duration));
    }
    println!(
        "Change to live on the {channel} channel: {:.1} s ({} the 60 s budget). Evidence: {}",
        change_to_live.as_secs_f64(),
        if within_budget { "within" } else { "OVER" },
        delivery_dir.join("delivery.json").display()
    );
    Ok(record)
}

fn run_channel(channel_root: &Path, channel: &str, susi_args: &[String]) -> Result<()> {
    validate_channel(channel)?;
    let current = current_checked(channel_root, channel)?;
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
    VerifyStartup(Option<PathBuf>),
    Deliver(DeliverOptions),
}

fn resolve_verify_startup_binary(
    channel_root: &Path,
    candidate: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(bin) = candidate {
        return Ok(bin);
    }
    if let Some(state) = read_state(&channel_state_path(channel_root))? {
        if let Some(current) = state.get("current").and_then(Value::as_str) {
            let path = PathBuf::from(current);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Ok(repo_root()?.join("target/debug").join(exe_name()))
}

fn verify_startup_gate(binary: &Path) -> Result<()> {
    if !binary.is_file() {
        return Err(format!("Expected native executable missing: {}", binary.display()).into());
    }
    let repo = repo_root()?;
    let status = Command::new("cargo")
        .current_dir(&repo)
        .args([
            "run",
            "--locked",
            "--offline",
            "-p",
            "xtask",
            "--bin",
            "startup-gate",
            "--",
            "--susi",
        ])
        .arg(binary)
        .arg("--skip-daemon-up")
        .status()?;
    if !status.success() {
        return Err(format!("local-channel startup-gate failed ({status})").into());
    }
    Ok(())
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
                    "verify-candidate" => Action::VerifyCandidate(None),
                    "verify-startup" => Action::VerifyStartup(None),
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
                let candidate = args
                    .get(idx + 1)
                    .filter(|s| !s.starts_with('-'))
                    .map(PathBuf::from);
                if candidate.is_some() {
                    idx += 1;
                }
                action = Some(Action::VerifyCandidate(candidate));
            }
            "verify-startup" if action.is_none() => {
                let candidate = args
                    .get(idx + 1)
                    .filter(|s| !s.starts_with('-'))
                    .map(PathBuf::from);
                if candidate.is_some() {
                    idx += 1;
                }
                action = Some(Action::VerifyStartup(candidate));
            }
            "deliver" if action.is_none() => {
                action = Some(Action::Deliver(DeliverOptions::default()))
            }
            "--prove-rollback" if matches!(action, Some(Action::Deliver(_))) => {
                if let Some(Action::Deliver(opts)) = action.as_mut() {
                    opts.prove_rollback = true;
                }
            }
            "--live-ref" if matches!(action, Some(Action::Deliver(_))) => {
                idx += 1;
                let val = args.get(idx).ok_or("missing value for --live-ref")?;
                if let Some(Action::Deliver(opts)) = action.as_mut() {
                    opts.live_ref = Some(val.clone());
                }
            }
            "--" if matches!(action, Some(Action::Deliver(_))) => {
                if let Some(Action::Deliver(opts)) = action.as_mut() {
                    opts.observe = args[idx + 1..].to_vec();
                }
                break;
            }
            other => {
                return Err(format!(
                    "unexpected argument `{other}` (usage: local-channel [status|build|rollback|run|verify-candidate|verify-startup|deliver [--prove-rollback] [--live-ref <ref>] [-- <susi args>]] [--channel dev] [--root <dir>])"
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
        Action::VerifyStartup(binary) => {
            let bin = resolve_verify_startup_binary(&channel_root, binary)?;
            verify_startup_gate(&bin)
        }
        Action::Deliver(opts) => {
            deliver(&channel_root, &channel, &opts)?;
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

    #[test]
    fn isolated_children_disable_registry_environment() {
        let mut command = Command::new("unused");
        configure_isolated_env(&mut command, Path::new("private"));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
                && value == Some(std::ffi::OsStr::new("1"))));
    }

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

    #[test]
    fn resolves_local_root_across_windows_macos_and_linux() -> Result<()> {
        assert_eq!(
            resolve_local_root_for(
                "windows",
                Some(Path::new("C:/Users/alice/AppData/Local")),
                Some(Path::new("C:/Users/alice")),
                None,
                None,
            )?,
            PathBuf::from("C:/Users/alice/AppData/Local").join("SusiLocal")
        );
        assert_eq!(
            resolve_local_root_for(
                "windows",
                None,
                Some(Path::new("C:/Users/alice")),
                None,
                None,
            )?,
            PathBuf::from("C:/Users/alice").join("AppData/Local/SusiLocal")
        );
        assert!(resolve_local_root_for("windows", None, None, None, None).is_err());

        assert_eq!(
            resolve_local_root_for("macos", None, None, Some(Path::new("/Users/alice")), None,)?,
            PathBuf::from("/Users/alice").join("Library/Application Support/SusiLocal")
        );
        assert!(resolve_local_root_for("macos", None, None, None, None).is_err());

        assert_eq!(
            resolve_local_root_for(
                "linux",
                None,
                None,
                Some(Path::new("/home/alice")),
                Some(Path::new("/custom/xdg")),
            )?,
            PathBuf::from("/custom/xdg").join("SusiLocal")
        );
        assert_eq!(
            resolve_local_root_for(
                "linux",
                None,
                None,
                Some(Path::new("/home/alice")),
                Some(Path::new("")),
            )?,
            PathBuf::from("/home/alice").join(".local/share/SusiLocal")
        );
        assert!(resolve_local_root_for("linux", None, None, None, None).is_err());
        Ok(())
    }

    #[test]
    fn parses_all_cli_actions_and_legacy_powershell_flags() -> Result<()> {
        let v =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| (*s).to_string()).collect() };

        let (act, chan, root) = parse_cli(&v(&["status", "--root", "/tmp/r"]))?;
        assert_eq!(act, Action::Status);
        assert_eq!(chan, "dev");
        assert_eq!(root, PathBuf::from("/tmp/r"));

        let (act, chan, root) = parse_cli(&v(&[
            "-Channel", "DEV", "-Root", "/tmp/ps", "-Action", "Rollback",
        ]))?;
        assert_eq!(act, Action::Rollback);
        assert_eq!(chan, "dev");
        assert_eq!(root, PathBuf::from("/tmp/ps"));

        let (act, _, _) = parse_cli(&v(&[
            "--root",
            "/tmp/r",
            "run",
            "--",
            "--version",
            "status",
        ]))?;
        assert_eq!(act, Action::Run(v(&["--version", "status"])));

        let (act, _, _) = parse_cli(&v(&[
            "verify-candidate",
            "target/debug/susi",
            "--root",
            "/tmp/r",
        ]))?;
        assert_eq!(
            act,
            Action::VerifyCandidate(Some(PathBuf::from("target/debug/susi")))
        );

        let (act, _, _) = parse_cli(&v(&[
            "verify-startup",
            "target/debug/susi",
            "--root",
            "/tmp/r",
        ]))?;
        assert_eq!(
            act,
            Action::VerifyStartup(Some(PathBuf::from("target/debug/susi")))
        );

        let (act, _, _) = parse_cli(&v(&["verify-startup", "--root", "/tmp/r"]))?;
        assert_eq!(act, Action::VerifyStartup(None));

        let (act, _, _) = parse_cli(&v(&["--action", "verify-startup", "--root", "/tmp/r"]))?;
        assert_eq!(act, Action::VerifyStartup(None));

        assert!(parse_cli(&v(&["unknown-subcommand", "--root", "/tmp/r"])).is_err());
        Ok(())
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn package(name: &str, dir: &str, lib: bool, bins: bool) -> PackageTargets {
        PackageTargets {
            member: affected::Member {
                name: name.to_string(),
                dir: dir.to_string(),
            },
            lib,
            bins,
        }
    }

    #[test]
    fn parses_deliver_options_and_observe_arguments() -> Result<()> {
        let (act, chan, root) = parse_cli(&strings(&["deliver", "--root", "/tmp/r"]))?;
        assert_eq!(act, Action::Deliver(DeliverOptions::default()));
        assert_eq!(chan, "dev");
        assert_eq!(root, PathBuf::from("/tmp/r"));

        let (act, _, _) = parse_cli(&strings(&[
            "--root",
            "/tmp/r",
            "deliver",
            "--prove-rollback",
            "--live-ref",
            "refs/susi/dev-live-lab",
            "--",
            "--help",
            "--prove-rollback",
        ]))?;
        assert_eq!(
            act,
            Action::Deliver(DeliverOptions {
                prove_rollback: true,
                live_ref: Some("refs/susi/dev-live-lab".to_string()),
                observe: strings(&["--help", "--prove-rollback"]),
            })
        );

        // Delivery flags belong to `deliver` only.
        assert!(parse_cli(&strings(&["build", "--prove-rollback"])).is_err());
        assert!(parse_cli(&strings(&["deliver", "--live-ref"])).is_err());
        Ok(())
    }

    #[test]
    fn owning_packages_picks_the_most_specific_member_and_refuses_global_inputs() -> Result<()> {
        let members = vec![
            package("susi", "", true, true),
            package("susi-paths", "crates/susi-paths", true, false),
            package("susi-core", "crates/susi-core", true, false),
            package("xtask", "xtask", false, true),
        ];
        let owners = owning_packages(
            &members,
            &strings(&[
                "crates/susi-paths/src/lib.rs",
                "src/cli/defs.rs",
                "docs/step1-acceptance.md",
            ]),
        )?;
        let names: Vec<&str> = owners.iter().map(|p| p.member.name.as_str()).collect();
        assert_eq!(names, ["susi", "susi-paths"]);

        let owners = owning_packages(&members, &strings(&["xtask/src/bin/local-channel.rs"]))?;
        let names: Vec<&str> = owners.iter().map(|p| p.member.name.as_str()).collect();
        assert_eq!(names, ["xtask"]);

        for global in [
            "Cargo.lock",
            "rust-toolchain.toml",
            "vendor/regex/src/lib.rs",
        ] {
            let Err(error) = owning_packages(&members, &strings(&["src/main.rs", global])) else {
                return Err(format!("{global} must need the full gate").into());
            };
            assert!(error.to_string().contains(global), "{error}");
            assert!(error.to_string().contains("local-workflow ship"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn unit_test_args_cover_only_targets_the_package_has() {
        assert_eq!(
            unit_test_args(&package("susi", "", true, true)),
            Some(strings(&[
                "test",
                "--locked",
                "--offline",
                "-q",
                "-p",
                "susi",
                "--lib",
                "--bins"
            ]))
        );
        assert_eq!(
            unit_test_args(&package("susi-paths", "crates/susi-paths", true, false)),
            Some(strings(&[
                "test",
                "--locked",
                "--offline",
                "-q",
                "-p",
                "susi-paths",
                "--lib"
            ]))
        );
        assert_eq!(
            unit_test_args(&package("xtask", "xtask", false, true)),
            Some(strings(&[
                "test",
                "--locked",
                "--offline",
                "-q",
                "-p",
                "xtask",
                "--bins"
            ]))
        );
        assert_eq!(
            unit_test_args(&package("fixtures", "fixtures", false, false)),
            None
        );
    }

    #[test]
    fn phase_clock_phases_sum_to_the_total() {
        let mut clock = PhaseClock::new();
        std::thread::sleep(Duration::from_millis(5));
        clock.mark("first");
        std::thread::sleep(Duration::from_millis(5));
        clock.mark("second");
        let names: Vec<&str> = clock.phases.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["first", "second"]);
        let sum: Duration = clock.phases.iter().map(|(_, d)| *d).sum();
        assert_eq!(sum, clock.total());
        assert!(clock.total() >= Duration::from_millis(10));
        assert!((millis(Duration::from_micros(1_234_567)) - 1234.6).abs() < f64::EPSILON);
    }

    #[test]
    fn drain_timed_collects_every_byte_and_notes_the_first() {
        let start = Instant::now();
        let data = vec![7_u8; 20_000];
        let (bytes, first) = drain_timed(std::io::Cursor::new(data.clone()), start)
            .join()
            .unwrap_or_default();
        assert_eq!(bytes, data);
        assert!(first.is_some());

        let (bytes, first) = drain_timed(std::io::Cursor::new(Vec::new()), start)
            .join()
            .unwrap_or_default();
        assert!(bytes.is_empty());
        assert!(first.is_none());
    }
}
