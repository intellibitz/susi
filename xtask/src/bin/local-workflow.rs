//! Local-only, cooperative worktree and scope reservations for parallel agents.
//! Reservations prevent overlaps between users of this launcher; they are not a
//! filesystem sandbox or a substitute for testing the integrated tree.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(windows)]
extern crate susi_windows_runtime;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[path = "../affected.rs"]
mod affected;
#[path = "../compiler_env.rs"]
mod compiler_env;
// Shared with the xtask binary, which runs the verifier; the ship tool only
// writes receipts, so the verification half is unused here.
#[allow(dead_code)]
#[path = "../distill_compose.rs"]
mod distill_compose;
#[allow(dead_code)]
#[path = "../gate_receipt.rs"]
mod gate_receipt;
#[path = "../native_provenance.rs"]
mod native_provenance;
// One source: the harness owns the storage guard and this tool compiles the same file.
#[path = "../../../crates/susi-harness/src/storage_guard.rs"]
mod storage_guard;
#[path = "../sysroot_sources.rs"]
mod sysroot_sources;
#[path = "../test_binary_evidence.rs"]
mod test_binary_evidence;
#[path = "../test_runner.rs"]
mod test_runner;
#[path = "../trusted_sysroot.rs"]
mod trusted_sysroot;
#[path = "../workspace_test_count.rs"]
mod workspace_test_count;
#[path = "../workspace_test_proof.rs"]
mod workspace_test_proof;
#[path = "../worktree_admin.rs"]
mod worktree_admin;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Resolve worktree-local Git paths together. Reject incomplete or ambiguous
/// output rather than silently omitting any operation guard.
fn git_paths(root: &Path, names: &[&str]) -> Result<Vec<PathBuf>> {
    let mut args = vec!["rev-parse", "--path-format=absolute"];
    for name in names {
        args.extend(["--git-path", *name]);
    }
    let output = git(root, &args)?;
    parse_git_paths(&output, names.len())
}

fn parse_git_paths(output: &str, expected: usize) -> Result<Vec<PathBuf>> {
    let paths: Vec<_> = output.lines().map(PathBuf::from).collect();
    if paths.len() != expected || paths.iter().any(|p| !p.is_absolute()) {
        return Err("Git returned incomplete or non-absolute operation paths".into());
    }
    Ok(paths)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|error| format!("git {} in {}: {error}", args.join(" "), dir.display()))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

fn text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "path must be valid UTF-8".into())
}

fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 48
        || !value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || value.starts_with('-')
        || value.ends_with('-')
    {
        return Err(
            "name must contain 1–48 lowercase ASCII letters, digits or interior hyphens".into(),
        );
    }
    if reserved(value) {
        return Err("Windows device names are not permitted".into());
    }
    Ok(())
}

fn reserved(value: &str) -> bool {
    let first = value.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(first.as_str(), "con" | "prn" | "aux" | "nul")
        || (first.len() == 4
            && (first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9'))
}

fn scope(value: &str) -> Result<String> {
    let value = value.replace('\\', "/");
    if value.is_empty() || !value.is_ascii() || value.starts_with('/') || value.ends_with('/') {
        return Err("scope must be a nonempty ASCII relative file or directory path".into());
    }
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.eq_ignore_ascii_case(".git")
            || component.ends_with('.')
            || component.ends_with(' ')
            || reserved(component)
            || component
                .chars()
                .any(|c| c.is_control() || ":*?\"<>|".contains(c))
        {
            return Err(format!("unsafe scope path: {value}").into());
        }
    }
    // Conservative on every platform so a claim shared with Windows is safe.
    Ok(value.to_ascii_lowercase())
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}

thread_local! {
    static HELD_LOCKS: std::cell::RefCell<BTreeSet<PathBuf>> =
        const { std::cell::RefCell::new(BTreeSet::new()) };
}

struct Lock {
    dir: PathBuf,
    key: PathBuf,
    _file: fs::File,
}
impl Lock {
    fn acquire(state: &Path) -> Result<Self> {
        fs::create_dir_all(state)?;
        let dir = state.join("lock");
        let key = state.canonicalize().unwrap_or_else(|_| state.to_path_buf());
        if HELD_LOCKS.with(|held| held.borrow().contains(&key)) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "local workflow is locked at {}; retry after the current operation",
                    dir.display()
                ),
            )
            .into());
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(state.join("lock.file"))?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    if dir.exists() && legacy_owner_alive(&dir) {
                        let _ = file.unlock();
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                    HELD_LOCKS.with(|held| {
                        held.borrow_mut().insert(key.clone());
                    });
                    let _ = fs::create_dir(&dir);
                    let _ = fs::write(
                        dir.join("owner"),
                        format!("pid={}\n{SLOTS_LAUNCHER}\n", std::process::id()),
                    );
                    return Ok(Self {
                        dir,
                        key,
                        _file: file,
                    });
                }
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() > std::time::Duration::from_secs(1800) {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "local workflow is locked at {}; timed out waiting for current operation",
                                dir.display()
                            ),
                        )
                        .into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
}

/// Marks a lock owner file written by a launcher with per-ship slots. Such a
/// launcher holds `integration` only around an `integrate` and moves the
/// accepted ref under `publish`; an owner without the mark is an older
/// launcher.
const SLOTS_LAUNCHER: &str = "launcher=slots";

/// The pid of a live launcher from before per-ship slots that holds
/// `integration`. Such a launcher holds it for a whole `ship` or `integrate`
/// gate, and its `integrate` moves `refs/susi/integration/current` without
/// taking `publish`; a slot ship's publication waits for it rather than race
/// it during the transition. A launcher with slots is never waited for here:
/// its `integrate` takes `publish` for its ref transaction.
fn legacy_integration_owner(workflow: &Path) -> Option<u32> {
    let dir = workflow.join("integration").join("lock");
    let text = fs::read_to_string(dir.join("owner")).ok()?;
    if text.lines().any(|line| line.trim() == SLOTS_LAUNCHER) || !legacy_owner_alive(&dir) {
        return None;
    }
    text.lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.trim().parse().ok())
}

fn wait_for_legacy_integration(workflow: &Path) {
    let mut announced = false;
    while let Some(pid) = legacy_integration_owner(workflow) {
        if !announced {
            eprintln!(
                "ship: waiting for pid {pid}, a launcher from before per-ship slots that holds the integration lock, before publishing"
            );
            announced = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn legacy_owner_alive(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("owner")) else {
        return false;
    };
    let Some(pid) = text
        .lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    if pid == std::process::id() {
        return false;
    }
    if cfg!(windows) {
        // Query the process object directly: no shell, console window, or
        // parsing of a process-list snapshot. Unreadable identity stays live.
        susi_config::process_probe::pid_alive(pid).unwrap_or(true)
    } else {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.dir.join("owner"));
        let _ = fs::remove_dir(&self.dir);
        HELD_LOCKS.with(|held| {
            held.borrow_mut().remove(&self.key);
        });
        let _ = self._file.unlock();
    }
}

/// An exclusive OS file lock on one directory: a worktree's `check`, or one
/// ship's verification slot. Held with `File::try_lock`, which the OS releases
/// when the holder exits or crashes, so a dead run never wedges anything and
/// no age-based stale-lock guess is needed.
struct HostGate {
    _file: fs::File,
}

/// Released explicitly: closing the file alone is not enough, because a
/// process spawned concurrently by another thread holds a copy of every open
/// descriptor until it execs, and that copy keeps a `flock` held.
impl Drop for HostGate {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Release a lock taken only to probe whether its holder is gone; see
/// [`HostGate`] for why the close alone is not enough.
fn release_probe(file: fs::File) {
    let _ = file.unlock();
}

/// Per-user state shared by every repository on this host:
/// `%LOCALAPPDATA%\susi-local-workflow` on Windows,
/// `~/.cache/susi-local-workflow` elsewhere.
fn host_state_dir() -> Result<PathBuf> {
    let base = env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .ok_or("cannot locate a per-user directory for local workflow state")?;
    Ok(if cfg!(windows) {
        base.join("susi-local-workflow")
    } else {
        base.join(".cache").join("susi-local-workflow")
    })
}

/// Wait for and take the host gate in `dir`; `holder` describes this run for
/// whoever waits next.
fn acquire_host_gate(dir: &Path, holder: &str) -> Result<HostGate> {
    fs::create_dir_all(dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join("gate.lock"))?;
    let started = std::time::Instant::now();
    let mut announced = false;
    loop {
        match file.try_lock() {
            Ok(()) => {
                // Informational only: correctness rests on the OS lock.
                let _ = fs::write(dir.join("owner"), format!("{holder}\n"));
                if announced {
                    eprintln!(
                        "host gate acquired after {:.0} s",
                        started.elapsed().as_secs_f64()
                    );
                }
                return Ok(HostGate { _file: file });
            }
            Err(fs::TryLockError::WouldBlock) => {
                if !announced {
                    let owner = fs::read_to_string(dir.join("owner")).unwrap_or_default();
                    eprintln!(
                        "waiting for the host gate ({}) held by {}",
                        dir.display(),
                        owner.trim()
                    );
                    announced = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

/// A running gate's marker in the host's live-gate registry
/// (`<host state>/live-gates`). Gates never wait for each other; instead each
/// cargo command of a gate takes an even share of the CPU, computed from how
/// many markers are alive when it starts (`gate_share`).
///
/// The marker is an OS-locked file: a crashed gate's lock is released by the
/// OS, and the next count removes its marker.
struct LiveGate {
    file: fs::File,
    path: PathBuf,
}

impl Drop for LiveGate {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

fn live_gate_dir() -> Result<PathBuf> {
    Ok(host_state_dir()?.join("live-gates"))
}

/// Register a running gate in `dir`. The marker is locked under a temporary
/// name and only then renamed to `*.gate`, so a concurrent count never sees
/// (and never removes as stale) a marker that is not locked yet.
fn register_live_gate(dir: &Path) -> Result<LiveGate> {
    fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let name = format!(
        "{}-{stamp}-{}",
        std::process::id(),
        LIVE_GATE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let pending = dir.join(format!("{name}.pending"));
    let file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&pending)?;
    if let Err(error) = file.try_lock() {
        drop(file);
        let _ = fs::remove_file(&pending);
        return Err(format!(
            "cannot lock live-gate marker {}: {error}",
            pending.display()
        )
        .into());
    }
    let path = dir.join(format!("{name}.gate"));
    if let Err(error) = fs::rename(&pending, &path) {
        let _ = file.unlock();
        drop(file);
        let _ = fs::remove_file(&pending);
        return Err(error.into());
    }
    Ok(LiveGate { file, path })
}

static LIVE_GATE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many gates are running on this host now, the caller's included (at
/// least 1). A marker whose lock can be taken belongs to a gate that exited or
/// crashed; it is removed. Entries that cannot be opened are not counted.
fn live_gates(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 1;
    };
    let mut live = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("gate") {
            continue;
        }
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                drop(file);
                let _ = fs::remove_file(&path);
            }
            Err(fs::TryLockError::WouldBlock) => live += 1,
            Err(fs::TryLockError::Error(_)) => {}
        }
    }
    live.max(1)
}

/// One gate command's share of the host when `live` gates run at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GateShare {
    live: usize,
    /// `CARGO_BUILD_JOBS`.
    jobs: usize,
    /// `RUST_TEST_THREADS` for `cargo test`.
    test_threads: usize,
    /// `NEXTEST_TEST_THREADS` for `cargo nextest run`.
    nextest_threads: usize,
}

/// Divide `cores` logical CPUs evenly between `live` gates, never below 2
/// jobs or test threads. A single gate keeps the settings it had when gates
/// queued: every core for cargo and `cargo test`, `GATE_TEST_THREADS` for
/// nextest. Lowering each gate's parallelism replaces the old host-wide
/// queue: the wall-clock deadline tests failed (2026-11-06) when two gates
/// each ran on every core, which an even share avoids.
fn gate_share(cores: usize, live: usize) -> GateShare {
    let live = live.max(1);
    let jobs = (cores.max(1) / live).max(2);
    GateShare {
        live,
        jobs,
        test_threads: jobs,
        nextest_threads: (GATE_TEST_THREADS / live).max(2),
    }
}

/// This host's share for a gate command starting now.
fn current_gate_share() -> GateShare {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let live = live_gate_dir().map_or(1, |dir| live_gates(&dir));
    gate_share(cores, live)
}

fn apply_gate_share(command: &mut Command, share: GateShare) {
    command
        .env("CARGO_BUILD_JOBS", share.jobs.to_string())
        .env("RUST_TEST_THREADS", share.test_threads.to_string())
        .env("NEXTEST_TEST_THREADS", share.nextest_threads.to_string());
}

/// Windows process-heavy commercial tests share launcher/handle resources in
/// addition to CPUs. Bound their libtest fan-out without reducing compiler
/// parallelism, test selection, or any deadline. Other gates keep their share.
fn commercial_command_share(mut share: GateShare, args: &[String], windows: bool) -> GateShare {
    let process_heavy = args.iter().any(|arg| arg == "--workspace")
        || args.windows(2).any(|pair| {
            matches!(pair, [flag, package]
                if (flag == "-p" || flag == "--package")
                    && matches!(package.as_str(), "susi-dev-workflow" | "susi-ecosystem-hub"))
        });
    if windows && args.first().map(String::as_str) == Some("test") && process_heavy {
        share.test_threads = share.test_threads.min(8);
    }
    share
}

fn print_gate_share(command: &str, share: GateShare) {
    eprintln!(
        "gate share for cargo {command}: {} live gate(s) on this host; {} build jobs, {} test threads",
        share.live, share.jobs, share.test_threads
    );
}

/// Register this process's gate on the host, or carry on unregistered (and
/// so uncounted) when the per-user state directory is unusable. The count is
/// a CPU-sharing input, never a correctness condition.
fn live_gate() -> Option<LiveGate> {
    match live_gate_dir().and_then(|dir| register_live_gate(&dir)) {
        Ok(gate) => Some(gate),
        Err(error) => {
            eprintln!("warning: gate not registered in the live-gate registry: {error}");
            None
        }
    }
}

/// One ship's private verification slot: candidate worktree
/// `<repo>-verify-candidate-<n>` beside the primary checkout, with its warm
/// target and attempt directory (gate logs, test home, native gate records)
/// under `<git common dir>/susi-local-workflow/slots/<n>`. Slots are taken
/// with a non-blocking OS lock: a busy slot is skipped, never waited for, so
/// concurrent ships verify side by side. Each slot keeps one candidate path,
/// so cargo's fingerprints stay valid and its target stays warm across runs.
struct Slot {
    candidate: PathBuf,
    attempt: PathBuf,
    _lock: HostGate,
}

const MAX_SLOTS: usize = 64;

fn acquire_slot(root: &Path) -> Result<Slot> {
    let slots = state(root)?.join("slots");
    fs::create_dir_all(&slots)?;
    let legacy = candidate_path(root)?;
    for index in 1..=MAX_SLOTS {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(slots.join(format!("{index}.lock")))?;
        match file.try_lock() {
            Ok(()) => {
                let mut candidate = legacy.clone().into_os_string();
                candidate.push(format!("-{index}"));
                let candidate = PathBuf::from(candidate);
                // A crashed or killed ship can leave files held open in its
                // candidate; take another slot instead of waiting for them.
                if let Err(error) = clear_candidate(root, &candidate) {
                    eprintln!("ship: skipping slot {index}: {error}");
                    continue;
                }
                let dir = slots.join(index.to_string());
                fs::create_dir_all(&dir)?;
                // Informational only: correctness rests on the OS lock.
                let _ = fs::write(dir.join("owner"), format!("pid={}\n", std::process::id()));
                return Ok(Slot {
                    candidate,
                    attempt: dir.join("attempt"),
                    _lock: HostGate { _file: file },
                });
            }
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Err(format!("all {MAX_SLOTS} verification slots are in use").into())
}

/// A narrowed gate that covers `xtask` still builds and tests its
/// `release-tooling` feature (local-release, prepare_release), which the
/// whole-workspace gate enables through `include_release_tooling`. Without it,
/// narrowing an xtask change to `-p xtask` silently drops those tests.
fn include_release_tooling_for_xtask(
    gates: &mut [Vec<String>],
    packages: &std::collections::BTreeSet<String>,
) {
    if !packages.contains("xtask") {
        return;
    }
    for args in gates {
        if matches!(
            args.first().map(String::as_str),
            Some("clippy" | "test" | "nextest")
        ) && !args.iter().any(|arg| arg == "xtask/release-tooling")
        {
            let at = args
                .iter()
                .position(|arg| arg == "--")
                .unwrap_or(args.len());
            args.splice(
                at..at,
                ["--features".to_owned(), "xtask/release-tooling".to_owned()],
            );
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Claim {
    name: String,
    worktree: PathBuf,
    base: String,
    scopes: Vec<String>,
}

fn read_claim(path: &Path) -> Result<Claim> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err("claim must not be a symlink".into());
    }
    let data = fs::read_to_string(path)?;
    let mut lines = data.lines();
    if lines.next() != Some("susi-local-claim-v2") {
        return Err("claim requires v2 with a recorded base; preserve legacy work and migrate its claim explicitly".into());
    }
    let claimant = lines.next().ok_or("missing claim name")?.to_owned();
    name(&claimant)?;
    let recorded = lines.next().ok_or("missing worktree")?;
    let worktree = PathBuf::from(recorded);
    if !worktree.is_absolute() && !absolute_on_some_host(recorded) {
        return Err("claim worktree must be absolute".into());
    }
    let base = lines.next().ok_or("missing claim base")?.to_owned();
    if !valid_oid(&base) {
        return Err("invalid claim base".into());
    }
    let scopes: Vec<String> = lines.map(scope).collect::<Result<_>>()?;
    if scopes.is_empty() {
        return Err("claim has no scopes".into());
    }
    if path.file_stem().and_then(|s| s.to_str()) != Some(&claimant) {
        return Err("claim filename mismatch".into());
    }
    Ok(Claim {
        name: claimant,
        worktree,
        base,
        scopes,
    })
}

/// The claim state lives in the shared `.git`, which one checkout may expose
/// to several operating systems. A worktree recorded by another OS is still
/// a valid claim, holding its scopes, but its path is not absolute here:
/// accept a Unix root or a Windows drive or UNC root, whichever host wrote it.
fn absolute_on_some_host(recorded: &str) -> bool {
    let bytes = recorded.as_bytes();
    recorded.starts_with('/')
        || recorded.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

impl Claim {
    /// Whether the claimed worktree path is an absolute path on this host.
    /// A claim recorded on another OS reserves its scopes here but its
    /// worktree can only be operated on from the host that recorded it.
    fn is_local(&self) -> bool {
        self.worktree.is_absolute()
    }

    fn host_label(&self) -> &'static str {
        if self.is_local() {
            ""
        } else {
            " [other host]"
        }
    }
}

/// Read a claim whose worktree must be operated on from this host.
fn read_local_claim(path: &Path) -> Result<Claim> {
    let claim = read_claim(path)?;
    if !claim.is_local() {
        return Err(format!(
            "claim {} was recorded on another host ({}); run this from that host",
            claim.name,
            claim.worktree.display()
        )
        .into());
    }
    Ok(claim)
}

fn claims(state: &Path) -> Result<Vec<Claim>> {
    let directory = state.join("claims");
    fs::create_dir_all(&directory)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        result.push(read_claim(&entry.path())?);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn state(root: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .join("susi-local-workflow"))
}

const INTEGRATION_REF: &str = "refs/susi/integration/current";

/// Wall-clock phases of the current `ship`/`integrate`, in order.
static PHASES: std::sync::Mutex<Vec<(String, u128)>> = std::sync::Mutex::new(Vec::new());

/// Run `work` as the named phase: print its wall-clock time on stderr and
/// remember it for the run summary, so every gate run shows where its minutes
/// went instead of leaving setup and publication unmeasured.
fn timed<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let value = work();
    record_phase(name, started.elapsed().as_millis());
    value
}

fn record_phase(name: &str, millis: u128) {
    eprintln!("phase {name}: {:.1} s", millis as f64 / 1000.0);
    if let Ok(mut phases) = PHASES.lock() {
        phases.push((name.to_owned(), millis));
    }
}

/// V2 separates actual elapsed operation time from inclusive, possibly nested
/// phase durations. Historical unversioned total_ms was only a phase sum.
fn timing_record(
    unix: u64,
    command: &str,
    outcome: &str,
    elapsed: std::time::Duration,
    phases: &[(String, u128)],
) -> serde_json::Value {
    serde_json::json!({
        "schema": "susi-local-workflow-timings-v2",
        "unix": unix,
        "command": command,
        "outcome": outcome,
        "elapsed_ms": elapsed.as_millis(),
        "elapsed_scope": "outer operation, including waits, retries, publication and cleanup; excluding summary serialization",
        "phases_ms": phases.iter().map(|(name, millis)| serde_json::json!([name, millis])).collect::<Vec<_>>(),
        "inclusive_phase_sum_ms": phases.iter().map(|(_, millis)| millis).sum::<u128>(),
    })
}

/// Print the phase table for this run and append it as one JSON line to
/// `<git common dir>/susi-local-workflow/timings.jsonl` (history for speed work).
fn finish_phases(root: &Path, command: &str, outcome: &str, elapsed: std::time::Duration) {
    let phases = match PHASES.lock() {
        Ok(mut phases) => std::mem::take(&mut *phases),
        Err(_) => return,
    };

    eprintln!("{command} timings ({outcome}):");
    for (name, millis) in &phases {
        eprintln!("  {name:<28} {:>8.1} s", *millis as f64 / 1000.0);
    }
    eprintln!(
        "  {:<28} {:>8.1} s",
        "elapsed (outer operation)",
        elapsed.as_secs_f64()
    );
    let Ok(dir) = state(root) else { return };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let line = timing_record(stamp, command, outcome, elapsed, &phases);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("timings.jsonl"))
    {
        let _ = writeln!(file, "{line}");
    }
}

use susi_build_policy as vendor_regressions;

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The fetched remote main (`refs/remotes/origin/main`), if this clone has it.
const FETCHED_MAIN_REF: &str = "refs/remotes/origin/main";

/// The commit a new claim starts from: the newest Susi landing. That is the
/// local integration ref (`ship`) or the fetched origin/main (`local-snapshot
/// submit`), whichever descends from the other; when they have diverged the
/// integration ref wins, as before. Without either, the primary HEAD.
fn claim_base(root: &Path) -> Result<String> {
    let integrated = integration_head(root)?;
    let fetched = fetched_main(root)?;
    Ok(match (integrated, fetched) {
        (Some(i), Some(f)) if i != f && is_ancestor(root, &i, &f)? => f,
        (Some(i), _) => i,
        (None, Some(f)) => f,
        (None, None) => git(root, &["rev-parse", "HEAD"])?,
    })
}

fn fetched_main(root: &Path) -> Result<Option<String>> {
    let fetched = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["rev-parse", "--verify", "--quiet", FETCHED_MAIN_REF])
        .output()?;
    if !fetched.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&fetched.stdout).trim().to_owned(),
    ))
}

/// Whether `head` has landed: `ship` records it in the local integration ref,
/// and `local-snapshot submit` (the development-main publisher) verifies it on
/// the remote main, which a fetch mirrors into `origin/main`. Either is
/// Susi's own landing, so either releases the reservation.
fn landed(root: &Path, head: &str) -> Result<bool> {
    if let Some(accepted) = integration_head(root)? {
        if is_ancestor(root, head, &accepted)? {
            return Ok(true);
        }
    }
    match fetched_main(root)? {
        Some(main) => is_ancestor(root, head, &main),
        None => Ok(false),
    }
}

fn integration_head(root: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["show-ref", "--verify", "--quiet", INTEGRATION_REF])
        .output()?;
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    if !out.status.success() {
        return Err("could not inspect local integration ref".into());
    }
    Ok(Some(git(root, &["rev-parse", INTEGRATION_REF])?))
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("cannot establish commit ancestry".into()),
    }
}

fn git_input(root: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing git stdin")?
        .write_all(input)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(strip_ansi(&String::from_utf8(out.stdout)?)
        .trim()
        .to_owned())
}

fn receipt_matches(
    root: &Path,
    reference: &str,
    receipt: &str,
    accepted: &str,
    tree: &str,
) -> Result<bool> {
    let lines: Vec<_> = receipt.lines().collect();
    let keys = [
        "agent=",
        "source=",
        "claim-base=",
        "previous=",
        "commit=",
        "tree=",
    ];
    if lines.len() < 8 || lines[0] != "susi-local-integration-v1" {
        return Ok(false);
    }
    let fields: Option<Vec<_>> = keys
        .iter()
        .enumerate()
        .map(|(index, key)| lines[index + 1].strip_prefix(key))
        .collect();
    let Some(fields) = fields else {
        return Ok(false);
    };
    if name(fields[0]).is_err()
        || ![fields[1], fields[2], fields[4], fields[5]]
            .iter()
            .all(|field| valid_oid(field))
        || fields[4] != accepted
        || fields[5] != tree
        || reference != format!("refs/susi/integration/receipts/{}/{}", fields[0], fields[1])
        || lines[7..].iter().all(|line| line.trim().is_empty())
        || !lines[7..]
            .iter()
            .any(|line| line.contains(" => exit code: 0;"))
        || lines[7..]
            .iter()
            .any(|line| keys.iter().any(|key| line.starts_with(key)))
        || lines[7..].iter().any(|line| {
            line.split_once(" => ")
                .is_some_and(|(_, status)| !status.starts_with("exit code: 0;"))
        })
    {
        return Ok(false);
    }
    if !is_ancestor(root, fields[2], fields[1])? || !is_ancestor(root, fields[1], accepted)? {
        return Ok(false);
    }
    if fields[3] != "none" {
        if !valid_oid(fields[3]) {
            return Ok(false);
        }
        if is_ancestor(root, fields[3], accepted)? {
            // A local integration ref left behind origin/main (main advanced
            // through ships whose receipts this clone does not hold) is in the
            // accepted history but can be older than the claim base. Receipts
            // written before `record_acceptance` recorded the publish base in
            // that case named the stale ref; accept them when the claim base
            // lies on the published lineage, the rule used for a displaced ref.
            if !is_ancestor(root, fields[2], fields[3])? {
                let Ok(publish_base) = git(root, &["rev-parse", &format!("{accepted}^1")]) else {
                    return Ok(false);
                };
                if !is_ancestor(root, fields[3], &publish_base)?
                    || !is_ancestor(root, fields[2], &publish_base)?
                {
                    return Ok(false);
                }
            }
        } else {
            // `ship` merges onto origin/main, so an integration ref that is not
            // in the accepted history was displaced, never built upon. Receipts
            // written before `record_acceptance` recorded the publish base in
            // that case named the displaced ref here; accept them only when the
            // claim base lies on the published lineage (the accepted merge's
            // first parent). This is no weaker than `previous=none`.
            let Ok(publish_base) = git(root, &["rev-parse", &format!("{accepted}^1")]) else {
                return Ok(false);
            };
            if !is_ancestor(root, fields[2], &publish_base)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn matching_receipt(root: &Path, accepted: &str, tree: &str) -> Result<Option<String>> {
    let listing = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/susi/integration/receipts/",
        ],
    )?;
    let mut entries = Vec::new();
    for entry in listing.lines() {
        let (reference, oid) = entry
            .split_once(' ')
            .ok_or("malformed receipt reference listing")?;
        if !valid_oid(oid) {
            return Err("invalid receipt blob oid".into());
        }
        entries.push((reference, oid));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("git cat-file stdin")?;
    let request: String = entries.iter().map(|(_, oid)| format!("{oid}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(request.as_bytes()));
    let output = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| "git cat-file writer panicked")??;
    if !output.status.success() {
        return Err(format!(
            "git cat-file --batch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let data = output.stdout;
    let mut at = 0;
    for (reference, oid) in entries {
        let end = data[at..]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("truncated git cat-file reply")?
            + at;
        let header = std::str::from_utf8(&data[at..end])?;
        let mut fields = header.split(' ');
        if fields.next() != Some(oid) || fields.next() != Some("blob") {
            return Err(format!("git cat-file did not return blob {oid}: {header}").into());
        }
        let size: usize = fields
            .next()
            .ok_or("git cat-file reply without size")?
            .parse()?;
        let body = end + 1;
        if data.len() < body + size + 1 || data[body + size] != b'\n' {
            return Err("truncated git cat-file blob".into());
        }
        let receipt = String::from_utf8_lossy(&data[body..body + size]);
        at = body + size + 1;
        if receipt_matches(root, reference, &receipt, accepted, tree)? {
            return Ok(Some(receipt.into_owned()));
        }
    }
    Ok(None)
}

fn scope_base(root: &Path, claim: &Claim, source: &str) -> Result<String> {
    let Some(accepted) = integration_head(root)? else {
        return Ok(claim.base.clone());
    };
    // A claim reserved at or after the accepted integration exempts no
    // integration history, so no receipt is needed to justify the base.
    if accepted == claim.base || is_ancestor(root, &accepted, &claim.base)? {
        return Ok(claim.base.clone());
    }
    if !is_ancestor(root, &claim.base, &accepted)? {
        return Err("accepted history does not descend from the reserved claim base".into());
    }
    let tree = git(root, &["rev-parse", &format!("{accepted}^{{tree}}")])?;
    if matching_receipt(root, &accepted, &tree)?.is_none() {
        return Err("accepted integration has no matching durable source/tree receipt".into());
    }
    // Only history shared with the tested integration is exempt. New changes
    // after that common ancestor remain subject to this worker's original scope.
    let bases = git(root, &["merge-base", "--all", source, &accepted])?;
    let bases: Vec<_> = bases.lines().collect();
    if bases.len() != 1 || !is_ancestor(root, &claim.base, bases[0])? {
        return Err("ambiguous or unrelated accepted merge base".into());
    }
    Ok(bases[0].to_owned())
}

fn check_source(root: &Path, claim: &Claim) -> Result<String> {
    if state(&claim.worktree)? != state(root)?
        || git(&claim.worktree, &["branch", "--show-current"])?
            != format!("local-agent/{}", claim.name)
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("integration requires a clean committed agent worktree".into());
    }
    let source = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if !is_ancestor(root, &claim.base, &source)? {
        return Err("source no longer descends from its reserved base".into());
    }
    let scope_base = scope_base(root, claim, &source)?;
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &scope_base,
            &source,
        ])
        .output()?;
    if !out.status.success() {
        return Err("cannot inspect committed scope".into());
    }
    for path in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
    {
        let path = scope(std::str::from_utf8(path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &path)) {
            return Err(format!("committed file is outside reserved scope: {path}").into());
        }
    }
    Ok(source)
}

fn sign_workspace_pass(root: &Path, key: &str) -> Result<String> {
    let signer = git(root, &["config", "--get", "user.signingkey"])?;
    let input = workspace_test_proof::signed_input(root, key)?;
    let out = workspace_test_proof::gpg(
        root,
        &["--local-user", &signer, "--armor", "--clearsign"],
        input.as_bytes(),
    )?;
    Ok(String::from_utf8(out.stdout)?)
}

/// Verify the exact merged checkout, using only native executable invocations.
/// The isolated home prevents tests from consuming the operator's runtime state;
/// Cargo and rustup retain their provisioned toolchain/cache locations.
/// The environment every gate command and every test binary of a gate gets:
/// a private home tree, the operator's real cargo and rustup homes, the
/// gate's target directory, no wrappers, and no credentials or endpoints
/// from the operator's session.
#[derive(Clone, Copy)]
struct GateEnv<'a> {
    home: &'a Path,
    cargo_home: &'a Path,
    rustup_home: &'a Path,
    target: &'a Path,
    sysroot: Option<&'a trusted_sysroot::VerifiedSysroot>,
}

fn apply_gate_env(command: &mut Command, gate: &GateEnv<'_>) -> Result<()> {
    compiler_env::apply_checked(command)?;
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
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
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    if let Some(sysroot) = gate.sysroot {
        sysroot.apply(command)?;
    }
    let home = gate.home;
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("CARGO_HOME", gate.cargo_home)
        .env("RUSTUP_HOME", gate.rustup_home)
        .env("CARGO_TARGET_DIR", gate.target)
        .env("CARGO_BUILD_TARGET_DIR", gate.target)
        .env("CARGO_BUILD_BUILD_DIR", gate.target)
        // Evidence logs are parsed; hosted CI exports CARGO_TERM_COLOR=always.
        .env("CARGO_TERM_COLOR", "never")
        // Host compiler wrappers may consult non-hermetic user-profile
        // configuration or run a shared daemon. The local gate is self-contained.
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "");
    Ok(())
}

/// An exit status with `code` for a gate whose outcome this process decided
/// (the runner's binaries each have their own status in the log).
fn exit_status(code: i32) -> std::process::ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(u32::from_ne_bytes(code.to_ne_bytes()))
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
}

/// A narrowed test gate run as Cargo's build plus the concurrent binary
/// runner (`test_runner`).
struct TestBinariesGate<'a> {
    candidate: &'a Path,
    attempt: &'a Path,
    /// The planned `cargo test ...` arguments (without `cargo`).
    args: &'a [String],
    host: &'a str,
    target: &'a Path,
    log_path: &'a Path,
    share: GateShare,
    env: GateEnv<'a>,
}

fn complete_workspace_requested() -> bool {
    // Kept explicit during migration. Unsupported hosts preserve serial
    // coverage; publication requires typed replay rather than a v1 proof.
    cfg!(windows) && env::var("SUSI_LOCAL_TEST_RUNNER").is_ok_and(|v| v == "workspace-binaries")
}

fn run_complete_binaries(
    gate: &TestBinariesGate<'_>,
) -> Result<(test_binary_evidence::Execution, serde_json::Value)> {
    let argv: Vec<String> = std::iter::once("cargo".into())
        .chain(gate.args.iter().cloned())
        .collect();
    let mut build = Command::new("cargo");
    apply_gate_env(&mut build, &gate.env)?;
    build.current_dir(gate.candidate).args(gate.args);
    apply_gate_share(&mut build, gate.share);
    let lanes = test_runner::concurrency(gate.share.jobs);
    let homes = gate.attempt.join("complete-test-homes");
    let durations = gate.attempt.join("complete-test-durations.json");
    let heavy = test_runner::heavy_binaries(gate.candidate);
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: gate.share.test_threads,
        build_jobs: test_runner::threads_per_binary(gate.share.jobs, lanes),
        home_root: &homes,
        target_dir: gate.target,
        triple: gate
            .args
            .windows(2)
            .find_map(|p| (p[0] == "--target").then_some(p[1].as_str())),
        durations: &durations,
        heavy: &heavy,
        log: gate.log_path,
    };
    let launch = |binary: &test_runner::Binary, home: &Path| {
        let mut command = Command::new(&binary.executable);
        let nested = home.join("target");
        apply_gate_env(
            &mut command,
            &GateEnv {
                home,
                target: &nested,
                ..gate.env
            },
        )?;
        Ok(command)
    };
    let retained = std::cell::RefCell::new(None);
    let execution = test_binary_evidence::execute(
        test_binary_evidence::ExecutionSpec {
            root: gate.candidate,
            argv: &argv,
            settings: &settings,
            callback_source: include_bytes!("local-workflow.rs"),
        },
        &mut build,
        &launch,
        &|plan, raw, metadata| {
            *retained.borrow_mut() = Some(native_provenance::retain_binary_plan(
                gate.candidate,
                plan,
                raw,
                metadata,
            )?);
            Ok(())
        },
    )?;
    Ok((
        execution,
        retained
            .into_inner()
            .ok_or("binary plan was not retained before execution")?,
    ))
}

/// Build the gate's test binaries with `cargo test ... --no-run`, then run
/// them through the concurrent runner, each in its own home under the
/// attempt directory. Returns the gate's status (0 only when the build and
/// every binary succeeded) and a note for the evidence line.
fn run_test_binaries(
    gate: &TestBinariesGate<'_>,
) -> Result<(std::process::ExitStatus, Option<String>)> {
    let mut log = fs::OpenOptions::new().append(true).open(gate.log_path)?;
    let mut build = Command::new("cargo");
    apply_gate_env(&mut build, &gate.env)?;
    build
        .current_dir(gate.candidate)
        .args(gate.args)
        .args(["--no-run", "--message-format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log.try_clone()?));
    apply_gate_share(&mut build, gate.share);
    let started = std::time::Instant::now();
    let output = build.output()?;
    record_phase("test build", started.elapsed().as_millis());
    let (binaries, diagnostics) =
        test_runner::parse_artifacts(&String::from_utf8_lossy(&output.stdout))?;
    for rendered in &diagnostics {
        log.write_all(rendered.as_bytes())?;
    }
    writeln!(
        log,
        "test build: cargo {} --no-run => exit code: {}; {} test binaries",
        gate.args.join(" "),
        output
            .status
            .code()
            .map_or_else(|| "none".to_owned(), |code| code.to_string()),
        binaries.len()
    )?;
    if !output.status.success() {
        return Ok((output.status, Some("runner=binaries:build failed".into())));
    }
    let lanes = test_runner::concurrency(gate.share.jobs);
    let heavy = test_runner::heavy_binaries(gate.candidate);
    let home_root = gate.attempt.join("test-homes");
    let durations = gate
        .attempt
        .parent()
        .unwrap_or(gate.attempt)
        .join("test-durations.json");
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: gate.share.test_threads,
        build_jobs: test_runner::threads_per_binary(gate.share.jobs, lanes),
        home_root: &home_root,
        target_dir: gate.target,
        triple: Some(gate.host),
        durations: &durations,
        heavy: &heavy,
        log: gate.log_path,
    };
    eprintln!(
        "test runner: {} binaries on up to {} lanes, {} aggregate test-thread budget",
        binaries.len(),
        lanes,
        settings.total_test_threads
    );
    let env = gate.env;
    let launch = move |binary: &test_runner::Binary, home: &Path| -> Result<Command> {
        let mut command = Command::new(&binary.executable);
        apply_gate_env(&mut command, &GateEnv { home, ..env })?;
        Ok(command)
    };
    let outcome = test_runner::run(&binaries, &settings, &launch)?;
    record_phase("test binaries", outcome.elapsed_ms);
    let _ = fs::remove_dir_all(&home_root);
    for (id, code) in &outcome.failed {
        eprintln!(
            "test runner: {id} failed ({})",
            code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}")
            )
        );
    }
    let complete = outcome.failed.is_empty() && outcome.ran == binaries.len();
    Ok((
        exit_status(i32::from(!complete)),
        Some(format!(
            "runner=binaries:{lanes} lanes, {} of {} ran, {} failed",
            outcome.ran,
            binaries.len(),
            outcome.failed.len()
        )),
    ))
}

const GIB: u64 = 1 << 30;

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// Free bytes a gate must find on the filesystem of its target before it
/// starts, by the workspace it gates. Measured 2026-11-07 on Linux x86_64: a
/// whole-workspace clippy and test gate of Susi core filled a cold slot target
/// to 13 GB, the same gate on a warm target added about 2 GB, a gate narrowed
/// to a few packages stayed under 2 GB, and the commercial workspace's whole
/// gate filled a cold target to 2.5 GB. A workspace this tool does not know
/// (a test fixture) needs only room to compile at all. A gate that runs out
/// of disk fails only after its compile time, with an error that reads like
/// a compiler bug, so a doomed gate is refused first.
fn gate_disk_floor(kind: Option<RepoKind>, whole_workspace: bool, warm: bool) -> u64 {
    match (kind, whole_workspace, warm) {
        (Some(RepoKind::Susi), true, false) => 12 * GIB,
        (Some(RepoKind::Susi), true, true) => 4 * GIB,
        (Some(RepoKind::Susi), false, _) => 2 * GIB,
        (Some(RepoKind::Commercial), true, false) => 3 * GIB,
        (Some(RepoKind::Commercial), _, _) => GIB,
        (None, _, _) => GIB / 2,
    }
}

/// Free bytes on the filesystem that holds `path`, or its nearest existing
/// ancestor when it does not exist yet. `None` when no mounted filesystem
/// claims the path: the check is then skipped, never a refusal.
fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path;
    let canonical = loop {
        if let Ok(canonical) = probe.canonicalize() {
            break canonical;
        }
        probe = probe.parent()?;
    };
    // On Windows `canonicalize` answers in verbatim form (`\\?\C:\...`),
    // which a mount point (`C:\`) is never a prefix of: compare the DOS form.
    let canonical = dos_path(&canonical);
    susi_config::host_metrics::filesystem_of(&canonical).map(|fs| fs.available_bytes)
}

/// `path` without Windows' verbatim prefix: `\\?\C:\x` becomes `C:\x` and
/// `\\?\UNC\host\share\x` becomes `\\host\share\x`; any other path is
/// returned as it is.
fn dos_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Whether a gate log says the disk ran out: ENOSPC on Linux and macOS, the
/// Windows ERROR_DISK_FULL text and code, and LLVM's output-stream failure,
/// which is how rustc reports a full disk while writing an object file.
fn gate_out_of_space(log: &[u8]) -> bool {
    let text = String::from_utf8_lossy(log);
    [
        "No space left on device",
        "os error 28)",
        "There is not enough space on the disk",
        "os error 112)",
        "IO failure on output stream",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) if meta.is_file() => meta.len(),
            _ => 0,
        })
        .sum()
}

/// Remove the caches the ship machinery owns and rebuilds on demand: the warm
/// targets stashed for idle slots, those slots' leftover candidate targets, and
/// the legacy integration target. A slot another ship holds locked is busy and
/// is left alone, as is this candidate's own target. Returns what was removed
/// with its size.
fn reclaim_gate_caches(workflow: &Path, candidate: &Path) -> Vec<(PathBuf, u64)> {
    let mut freed = Vec::new();
    let mut remove = |path: PathBuf| {
        if path.is_dir() {
            let size = dir_size(&path);
            if fs::remove_dir_all(&path).is_ok() {
                freed.push((path, size));
            }
        }
    };
    remove(workflow.join("integration-target"));
    let name = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (stem, current) = name.rsplit_once("-verify-candidate-").unwrap_or((name, ""));
    let slots = workflow.join("slots");
    let Ok(entries) = fs::read_dir(&slots) else {
        return freed;
    };
    for entry in entries.flatten() {
        let slot = entry.file_name();
        let Some(slot) = slot.to_str() else {
            continue;
        };
        if slot.is_empty() || !slot.bytes().all(|byte| byte.is_ascii_digit()) || slot == current {
            continue;
        }
        // The slot's lock, taken without waiting: a busy slot is another ship's.
        let Ok(lock) = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(slots.join(format!("{slot}.lock")))
        else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        remove(entry.path().join("target"));
        if let Some(parent) = candidate.parent() {
            remove(
                parent
                    .join(format!("{stem}-verify-candidate-{slot}"))
                    .join("target"),
            );
        }
        release_probe(lock);
    }
    freed
}

/// Refuse a gate whose filesystem is too full to finish, after reclaiming the
/// ship's own idle caches. A gate that cannot be measured is not refused.
fn require_gate_disk(candidate: &Path, target: &Path, whole_workspace: bool) -> Result<()> {
    let warm = target.join("CACHEDIR.TAG").is_file();
    let floor = gate_disk_floor(repo_kind(candidate).ok(), whole_workspace, warm);
    let Some(free) = free_space(candidate) else {
        return Ok(());
    };
    if free >= floor {
        return Ok(());
    }
    let workflow = state(candidate)?;
    for (path, size) in reclaim_gate_caches(&workflow, candidate) {
        eprintln!("gate: reclaimed {} ({})", path.display(), gib(size));
    }
    let Some(free) = free_space(candidate) else {
        return Ok(());
    };
    if free >= floor {
        return Ok(());
    }
    Err(format!(
        "gate refused before compiling: {} free on the filesystem holding {}, and a {} gate on a {} target needs about {}; free space (worktree target/ directories, old candidates) and run ship again",
        gib(free),
        candidate.display(),
        if whole_workspace { "whole-workspace" } else { "narrowed" },
        if warm { "warm" } else { "cold" },
        gib(floor)
    )
    .into())
}

/// A gate that ran out of disk runs once more after the ship's idle caches
/// are reclaimed: the first attempt's log is kept beside the rerun's, and the
/// command is pointed at a fresh log. `None` when nothing could be reclaimed,
/// so the gate fails as it did.
fn reclaim_for_rerun(
    candidate: &Path,
    log_path: &Path,
    command: &mut Command,
    gate: &str,
) -> Result<Option<String>> {
    let workflow = state(candidate)?;
    let freed = reclaim_gate_caches(&workflow, candidate);
    if freed.is_empty() {
        return Ok(None);
    }
    let total: u64 = freed.iter().map(|(_, size)| *size).sum();
    for (path, size) in &freed {
        eprintln!("gate {gate}: reclaimed {} ({})", path.display(), gib(*size));
    }
    eprintln!(
        "gate {gate}: out of disk space; {} reclaimed, running it again",
        gib(total)
    );
    let _ = fs::rename(log_path, log_path.with_extension("out-of-space.log"));
    let log = fs::File::create(log_path)?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    Ok(Some(format!(
        "reclaim gate={gate} freed-bytes={total} caches={}",
        freed.len()
    )))
}

/// The disk-full prefix of a failed gate's error, or nothing.
fn out_of_space_prefix(candidate: &Path, log: &[u8]) -> String {
    if !gate_out_of_space(log) {
        return String::new();
    }
    format!(
        "out of disk space ({} free on the filesystem holding {}); ",
        free_space(candidate).map_or_else(|| "unknown".to_owned(), gib),
        candidate.display()
    )
}

fn verify_candidate(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
    // No queue: this gate runs now, beside any other, and each of its cargo
    // commands takes an even share of the host (`gate_share`).
    let _live = live_gate();
    timed("provenance check", || {
        native_provenance::require_unmodified_history(candidate)
    })
    .map_err(|error| format!("provenance: {error}"))?;
    let compiler = Command::new("rustc")
        .current_dir(candidate)
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc in {}: {error}", candidate.display()))?;
    if !compiler.status.success() {
        return Err("cannot identify native compiler".into());
    }
    let compiler = String::from_utf8(compiler.stdout)?;
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("compiler did not report host")?;
    if cfg!(windows) && !host.ends_with("-pc-windows-msvc") {
        return Err("Windows integration requires a native MSVC compiler".into());
    }
    let original_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("cannot identify toolchain home")?;
    let home = attempt.join("test-home");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    let target = timed("restore warm target", || {
        restore_candidate_target(candidate)
    });
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let gates: &[&[&str]] = &[
        &["fmt", "--all", "--check"],
        &["deny", "--offline", "--locked", "check", "licenses"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
            "--",
            "-D",
            "warnings",
        ],
        &[
            "nextest",
            "run",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            host,
            "--target-dir",
            text(&target)?,
        ],
    ];
    let mut gates: Vec<Vec<String>> = gates
        .iter()
        .map(|args| args.iter().map(|arg| (*arg).into()).collect())
        .collect();
    let nextest = nextest_requested();
    select_test_runner(&mut gates[3], nextest);
    let (selection, libraries) =
        timed("affected selection", || gate_selection(candidate, attempt))?;
    match &selection {
        affected::Selection::Packages(packages) => {
            for args in &mut gates {
                if matches!(
                    args.first().map(String::as_str),
                    Some("clippy" | "test" | "nextest")
                ) {
                    affected::narrow(args, packages);
                }
            }
            include_release_tooling_for_xtask(&mut gates, packages);
            eprintln!(
                "local gate: {} of the workspace packages ({})",
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        affected::Selection::Workspace(reason) => {
            eprintln!("local gate: whole workspace ({reason})");
        }
    }
    vendor_regressions::include_release_tooling(candidate, &mut gates);
    timed("gate disk check", || {
        require_gate_disk(
            candidate,
            &target,
            matches!(selection, affected::Selection::Workspace(_)),
        )
    })?;
    if matches!(selection, affected::Selection::Workspace(_))
        && candidate.join("crates/susi-core/Cargo.toml").is_file()
    {
        // `include_release_tooling` recognizes clippy and test; the nextest
        // workspace gate needs the same feature.
        let all: BTreeSet<String> = ["xtask".to_owned()].into();
        include_release_tooling_for_xtask(&mut gates, &all);
    }
    // A narrowed `cargo test` gate runs its binaries through the concurrent
    // runner, which (like nextest) leaves doctests to a companion gate. The
    // whole-workspace gate keeps serial `cargo test`: its signed test proof
    // is keyed on that exact invocation.
    let complete_workspace = !nextest
        && complete_workspace_requested()
        && matches!(selection, affected::Selection::Workspace(_));
    if complete_workspace {
        test_binary_evidence::require_supported_source_inputs()?;
    }
    let parallel_tests = !nextest
        && !test_runner::serial_requested()
        && (matches!(selection, affected::Selection::Packages(_)) || complete_workspace);
    let doc = if complete_workspace {
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[3].iter().cloned())
            .collect();
        let docs = test_binary_evidence::doc_argv(&logical)?;
        gates[3] = test_binary_evidence::build_argv(&logical)?[1..].to_vec();
        Some(docs[1..].to_vec())
    } else if nextest {
        doc_gate(&gates[3], libraries.as_ref())
    } else if parallel_tests {
        // `doc_gate` skips the `nextest run` prefix; give the `test` gate
        // the same two-word prefix so its package flags survive.
        let mut as_runner: Vec<String> = vec!["test".into(), "run".into()];
        as_runner.extend(gates[3].iter().skip(1).cloned());
        doc_gate(&as_runner, libraries.as_ref())
    } else {
        None
    };
    // The standalone vendored adapters change only with `vendor/` or the
    // toolchain, both of which select the whole workspace.
    if matches!(selection, affected::Selection::Workspace(_)) {
        gates.extend(timed("vendored adapter plan", || {
            vendor_regressions::commands(candidate, host, &target)
        })?);
    }
    let mut gate_ids: Vec<String> = (0..gates.len())
        .map(|index| match index {
            0 => "format".into(),
            1 => "licenses".into(),
            2 => "clippy".into(),
            3 => "workspace-tests".into(),
            _ => format!("integration:{index}"),
        })
        .collect();
    let inputs: serde_json::Value =
        serde_json::from_slice(&fs::read(attempt.join("native-inputs.json"))?)?;
    for input in inputs
        .as_array()
        .ok_or("invalid native integration inputs")?
    {
        let worker = input["worker"].as_str().ok_or("missing native worker")?;
        let source = input["source"].as_str().ok_or("missing native source")?;
        let scopes = input["scopes"]
            .as_array()
            .ok_or("missing native scopes")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("invalid native scope")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let task = timed("task acceptance plan", || {
            native_provenance::task_for_claim(candidate, source, worker, &scopes)
        })?;
        let mut args = task.argv[1..].to_vec();
        args.extend([
            "--target".into(),
            host.into(),
            "--target-dir".into(),
            text(&target)?.into(),
        ]);
        gate_ids.push(format!("task:{}", task.id));
        gates.push(args);
    }
    if let Some(doc) = doc {
        gate_ids.push("doc-tests".into());
        gates.push(doc);
    }
    let plan: Vec<_> = gate_ids
        .iter()
        .zip(&gates)
        .map(|(id, args)| {
            let argv: Vec<_> = std::iter::once("cargo".to_owned())
                .chain(args.iter().cloned())
                .collect();
            let mut entry = serde_json::json!({"id":id,"argv":argv});
            if complete_workspace && id == "workspace-tests" {
                entry["execution"] = serde_json::json!({"schema":test_binary_evidence::SCHEMA});
            }
            entry
        })
        .collect();
    let mut native_gates = Vec::new();
    let mut pending_binary: Option<(usize, test_binary_evidence::Execution, serde_json::Value)> =
        None;
    let mut pending_workspace_proof: Option<(String, Command)> = None;
    let mut sysroot = None;
    let mut evidence = Vec::new();
    for (index, args) in gates.iter().enumerate() {
        if sysroot.is_none()
            && matches!(
                args.first().map(String::as_str),
                Some("clippy" | "test" | "build")
            )
            && candidate.join("crates/susi-core/Cargo.toml").is_file()
        {
            sysroot = Some(timed("trusted sysroot", || {
                trusted_sysroot::prepare(candidate)
            })?);
        }
        let log_path = attempt.join(format!("gate-{index}.log"));
        let log = fs::File::create(&log_path)?;
        let started = std::time::Instant::now();
        let gate_target = args
            .windows(2)
            .find_map(|pair| (pair[0] == "--target-dir").then(|| PathBuf::from(&pair[1])))
            .unwrap_or_else(|| target.clone());
        let gate_env = GateEnv {
            home: &home,
            cargo_home: &cargo_home,
            rustup_home: &rustup_home,
            target: &gate_target,
            sysroot: sysroot.as_ref(),
        };
        let mut command = Command::new("cargo");
        apply_gate_env(&mut command, &gate_env)?;
        command
            .current_dir(candidate)
            .args(args)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let share = current_gate_share();
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let measure = |cmd: &Command| {
            sysroot
                .as_ref()
                .filter(|_| index == 3 && !parallel_tests)
                .and_then(|s| {
                    workspace_test_proof::fingerprint(
                        candidate,
                        args,
                        workspace_test_proof::Toolchain {
                            compiler: &compiler,
                            host,
                            sysroot: &s.evidence,
                        },
                        cmd,
                        &home,
                    )
                    .ok()
                })
        };
        let test_key = measure(&command);
        let binaries_gate = parallel_tests
            && gate_ids
                .get(index)
                .is_some_and(|id| id == "workspace-tests")
            && args.first().map(String::as_str) == Some("test");
        let (mut status, runner_note) = if binaries_gate && complete_workspace {
            let (execution, descriptor) = run_complete_binaries(&TestBinariesGate {
                candidate,
                attempt,
                args,
                host,
                target: &gate_target,
                log_path: &log_path,
                share,
                env: gate_env,
            })?;
            pending_binary = Some((index, execution, descriptor));
            (
                exit_status(0),
                Some("runner=complete-inventory-v1; companion doctests pending".into()),
            )
        } else if binaries_gate {
            // Cargo builds the binaries; the runner executes them beside
            // each other. The planned `cargo test` argv stays the gate's
            // identity in the evidence and the receipt. (`command` stays
            // for the proof fingerprint re-measure below; it never runs.)
            run_test_binaries(&TestBinariesGate {
                candidate,
                attempt,
                args,
                host,
                target: &gate_target,
                log_path: &log_path,
                share,
                env: gate_env,
            })?
        } else {
            (command.status()?, None)
        };
        // Out of disk mid-gate: reclaim the ship's idle caches and run the
        // command once more (serial gates only; the runner's binaries each
        // report their own status in the log).
        if !binaries_gate
            && !status.success()
            && gate_out_of_space(&fs::read(&log_path).unwrap_or_default())
        {
            let gate = gate_ids.get(index).map_or("?", String::as_str);
            if let Some(line) = reclaim_for_rerun(candidate, &log_path, &mut command, gate)? {
                evidence.push(line);
                status = command.status()?;
            }
        }
        // ExitStatus Display differs across OSes ("exit status" on Unix).
        // Durable receipts use one explicit numeric wire representation.
        let status_text = status
            .code()
            .map_or_else(|| status.to_string(), |code| format!("exit code: {code}"));
        let mut result = format!(
            "cargo {} => {status_text}; elapsed-ms={}",
            args.join(" "),
            started.elapsed().as_millis()
        );
        record_phase(
            &format!("gate {}", gate_ids.get(index).map_or("?", String::as_str)),
            started.elapsed().as_millis(),
        );
        let doc_tests = args.iter().any(|arg| arg == "--doc");
        if status.success() && !doc_tests {
            let log_path = attempt.join(format!("gate-{index}.log"));
            let counted = match args.first().map(String::as_str) {
                Some("test") => Some(workspace_test_count::verified_pass_count(&log_path)),
                Some("nextest") => {
                    Some(workspace_test_count::verified_nextest_pass_count(&log_path))
                }
                _ => None,
            };
            if let Some(counted) = counted {
                let passed = counted.map_err(|error| {
                    format!("gate failed: {result}; {}: {error}", log_path.display())
                })?;
                result.push_str(&format!("; observed-passes={passed}"));
            }
        }
        if let Some(note) = runner_note {
            result.push_str(&format!("; {note}"));
        }
        evidence.push(result.clone());
        evidence.push(format!(
            "gate-share={} live={} jobs={} test-threads={}",
            gate_ids.get(index).map_or("?", String::as_str),
            share.live,
            share.jobs,
            share.test_threads
        ));
        fs::write(attempt.join("verification.txt"), evidence.join("\n"))?;
        if !status.success() {
            let log_bytes = fs::read(&log_path)?;
            let failed_log = native_provenance::store_blob(candidate, &log_bytes)?;
            fs::write(
                attempt.join(format!("gate-{index}.log.oid")),
                format!("{failed_log}\n"),
            )?;
            return Err(format!(
                "gate failed: {}{result}; log-blob={failed_log}; logs retained at {}",
                out_of_space_prefix(candidate, &log_bytes),
                attempt.display(),
            )
            .into());
        }
        // A serial proof cannot attest to a substituted binary runner. Keep the
        // command that actually ran, but publish nothing until every planned
        // gate (including any companion doctests) and the final recheck pass.
        if !binaries_gate {
            if let Some(key) = test_key.filter(|key| measure(&command).as_ref() == Some(key)) {
                pending_workspace_proof = Some((key, command));
            }
        }
        let argv: Vec<_> = std::iter::once("cargo".to_owned())
            .chain(args.iter().cloned())
            .collect();
        native_gates.push(native_provenance::gate(
            candidate,
            native_provenance::GateExecution {
                id: &gate_ids[index],
                argv: &argv,
                status,
                elapsed_ms: started.elapsed().as_millis(),
                log: &log_path,
            },
        )?);
        fs::write(
            attempt.join("native-gates.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"plan":plan,"gates":native_gates}))?,
        )?;
    }
    if let Some(sysroot) = &sysroot {
        sysroot.recheck()?;
        evidence.push(format!(
            "sysroot={}",
            serde_json::to_string(&sysroot.evidence)?
        ));
    }
    if let Some((index, execution, mut descriptor)) = pending_binary {
        native_provenance::finish_binary_plan(candidate, &mut descriptor, &execution)?;
        native_gates[index]["execution"] = descriptor;
        // Preliminary per-gate time. The enclosing phase below also includes
        // final receipt persistence and the last input/artifact guard pass.
        native_gates[index]["elapsed_ms"] = serde_json::json!(execution.elapsed_ms());
        fs::write(
            attempt.join("native-gates.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"plan":plan,"gates":native_gates}))?,
        )?;
        execution.recheck()?;
        record_phase(
            "complete workspace binaries including persistence and final checks",
            execution.elapsed_ms(),
        );
        evidence.push(format!(
            "complete-binaries-inclusive-ms={}",
            execution.elapsed_ms()
        ));
    }
    if let (Some((key, command)), Some(sysroot)) = (pending_workspace_proof, sysroot.as_ref()) {
        let remeasured = workspace_test_proof::fingerprint(
            candidate,
            &gates[3],
            workspace_test_proof::Toolchain {
                compiler: &compiler,
                host,
                sysroot: &sysroot.evidence,
            },
            &command,
            &home,
        )
        .ok();
        if let Some(record) =
            completed_workspace_proof(&key, remeasured.as_deref(), native_gates.len(), plan.len())
        {
            evidence.push(record);
        }
    }
    evidence.push(match &selection {
        affected::Selection::Packages(packages) => format!(
            "gate-selection=packages:{}",
            packages.iter().cloned().collect::<Vec<_>>().join(",")
        ),
        affected::Selection::Workspace(reason) => format!("gate-selection=workspace:{reason}"),
    });
    Ok(evidence)
}

// Completion is deliberately independent of test counts: every planned gate
// must succeed, and the exact execution inputs must still match at publication.
fn completed_workspace_proof(
    key: &str,
    remeasured: Option<&str>,
    completed: usize,
    planned: usize,
) -> Option<String> {
    (planned > 0 && completed == planned && remeasured == Some(key))
        .then(|| workspace_test_proof::record(key))
}
/// Which packages the clippy and test gates cover: those the change since the
/// recorded integration base can affect. Without a recorded base, or with
/// `SUSI_LOCAL_GATE=full`, the whole workspace. Production qualification
/// (`local-release qualify`) always runs the whole workspace.
/// The gate's package selection, and the workspace's library packages when
/// they were loaded (the doctest gate may only name those).
type GateSelection = (affected::Selection, Option<BTreeSet<String>>);

fn gate_selection(candidate: &Path, attempt: &Path) -> Result<GateSelection> {
    if env::var("SUSI_LOCAL_GATE").is_ok_and(|mode| mode == "full") {
        return Ok((
            affected::Selection::Workspace("SUSI_LOCAL_GATE=full".into()),
            None,
        ));
    }
    let Ok(base) = fs::read_to_string(attempt.join("selection-base")) else {
        return Ok((
            affected::Selection::Workspace("no recorded base".into()),
            None,
        ));
    };
    let base = base.trim();
    if !valid_oid(base) {
        return Err("recorded selection base is not a commit oid".into());
    }
    let changed = affected::changed(candidate, base, "HEAD")?;
    let workspace = affected::load(candidate)?;
    Ok((
        affected::select(&workspace, &changed),
        Some(workspace.libraries.clone()),
    ))
}

/// Test threads for nextest gates: enough to finish the suite in parallel,
/// fewer than the machine's logical cores so the process-spawning tests keep
/// the CPU their wall-clock deadlines assume (see `.config/nextest.toml`).
const GATE_TEST_THREADS: usize = 16;

/// Whether the test gate runs through `cargo nextest` instead of `cargo test`:
/// opt-in with `SUSI_LOCAL_TEST_RUNNER=nextest`. Measured 2026-11-06 on the
/// same 7 packages, nextest ran the tests in 351 s against 736 s, but 2 of the
/// 3 ships that used it on main failed on tests that pass under `cargo test`
/// (a susi-config concurrent-writer test, a susi-gmcp fail-closed test, and
/// two deadline tests under CPU contention). The suite coordinates tests with
/// 21 process-local static locks (`AUDIT_TEST_LOCK`, `ENV_LOCK`, ...), which
/// one process per test cannot honour; until that coordination is
/// cross-process safe, `cargo test` stays the default.
fn nextest_requested() -> bool {
    env::var("SUSI_LOCAL_TEST_RUNNER").is_ok_and(|runner| runner == "nextest")
}

/// Turn a `nextest run ...` test gate into `test ...` unless nextest was
/// requested. The rest of the arguments (selection, target, features) are the
/// same for both runners.
fn select_test_runner(gate: &mut Vec<String>, nextest: bool) {
    if !nextest && gate.first().map(String::as_str) == Some("nextest") {
        gate.splice(..2, ["test".to_owned()]);
    }
}

/// The doctest companion of a nextest test gate (nextest does not run
/// doctests), narrowed to the selected packages that have a library target.
/// `None` when the selection holds no library, so there is nothing to run.
fn doc_gate(test_gate: &[String], libraries: Option<&BTreeSet<String>>) -> Option<Vec<String>> {
    let mut gate: Vec<String> = vec!["test".into(), "--doc".into()];
    let mut packages = Vec::new();
    let mut features = Vec::new();
    let mut rest = test_gate.iter().skip(2).peekable();
    while let Some(arg) = rest.next() {
        if arg == "-p" {
            let package = rest.next()?;
            if libraries.is_none_or(|libs| libs.contains(package)) {
                packages.push(package.clone());
            }
            continue;
        }
        if arg == "--features" {
            features.push(rest.next()?.clone());
            continue;
        }
        gate.push(arg.clone());
    }
    let whole = test_gate.iter().any(|arg| arg == "--workspace");
    // Keep a `pkg/feature` only when its package is still selected: cargo
    // refuses a feature of a package the narrowed doctest gate dropped.
    for feature in features {
        let keep = whole
            || feature
                .split_once('/')
                .is_none_or(|(package, _)| packages.iter().any(|kept| kept == package));
        if keep {
            gate.push("--features".into());
            gate.push(feature);
        }
    }
    if !whole {
        if packages.is_empty() {
            return None;
        }
        // Keep the package flags where narrowing put them: before `--offline`.
        let at = gate
            .iter()
            .position(|arg| arg == "--offline")
            .unwrap_or(gate.len());
        let flags: Vec<String> = packages
            .into_iter()
            .flat_map(|package| ["-p".to_owned(), package])
            .collect();
        gate.splice(at..at, flags);
    }
    Some(gate)
}

/// Serialize only integration, not agent edits/commits. Publish the tested commit
/// and its receipt together in one Git ref transaction. No primary checkout moves.
fn integrate_with<F>(root: &Path, claimants: &[&str], verify: F) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    integrate_many_with(root, claimants, verify)
}

fn integrate_many_with<F>(root: &Path, claimants: &[&str], verify: F) -> Result<String>
where
    F: FnOnce(&Path, &Path) -> Result<Vec<String>>,
{
    native_provenance::require_unmodified_history(root)?;
    let first = *claimants
        .first()
        .ok_or("at least one claimant is required")?;
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    let state = state(root)?;
    let _integration = Lock::acquire(&state.join("integration"))?;
    let inputs = {
        let _claims = Lock::acquire(&state)?;
        claimants
            .iter()
            .map(|claimant| {
                let path = state.join("claims").join(format!("{claimant}.claim"));
                let claim = read_local_claim(&path)?;
                let source = check_source(root, &claim)?;
                let bytes = fs::read(&path)?;
                Ok((claim, source, path, bytes))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let previous = integration_head(root)?;
    let base = previous.clone().unwrap_or_else(|| inputs[0].0.base.clone());
    // Dependency chains must be rooted in accepted history. Two unaccepted
    // workers cannot authorize each other's common, unowned starting commit.
    let mut admitted = vec![false; inputs.len()];
    loop {
        let before = admitted.clone();
        for (index, (claim, _, _, _)) in inputs.iter().enumerate() {
            if is_ancestor(root, &claim.base, &base)? {
                admitted[index] = true;
            }
            for (other, (_, source, _, _)) in inputs.iter().enumerate() {
                if before[other] && is_ancestor(root, &claim.base, source)? {
                    admitted[index] = true;
                }
            }
        }
        if admitted.iter().all(|value| *value) {
            break;
        }
        if admitted == before {
            return Err(
                "integration history does not contain the claim base; reconcile explicitly".into(),
            );
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let attempt = state
        .join("attempts")
        .join(format!("{first}-{stamp}-{}", std::process::id()));
    fs::create_dir_all(&attempt)?;
    let candidate = candidate_path(root)?;

    clear_candidate(root, &candidate)?;
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "--detach",
            text(&candidate)?,
            &base,
        ],
    )?;
    fs::write(
        attempt.join("inputs.txt"),
        inputs
            .iter()
            .map(|(claim, source, _, _)| {
                format!(
                    "agent={}\nclaim-base={}\nsource={source}\nintegration-base={base}\n",
                    claim.name, claim.base
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    fs::write(attempt.join("selection-base"), format!("{base}\n"))?;
    fs::write(
        attempt.join("native-inputs.json"),
        serde_json::to_vec_pretty(
            &inputs
                .iter()
                .map(|(claim, source, _, _)| {
                    serde_json::json!({
                        "worker":claim.name,"source":source,"scopes":claim.scopes
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    for (_, source, _, _) in &inputs {
        let merged = git(
            &candidate,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "user.name=Susi Local Integration",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "--no-edit",
                source,
            ],
        );
        if let Err(error) = merged {
            fs::write(attempt.join("status"), format!("merge failed: {error}\n"))?;
            return Err(format!(
                "merge failed; source and reservation retained; repair evidence at {}: {error}",
                candidate.display()
            )
            .into());
        }
    }
    let head = git(&candidate, &["rev-parse", "HEAD"])?;
    let tree = git(&candidate, &["rev-parse", "HEAD^{tree}"])?;
    let mut evidence = match verify(&candidate, &attempt) {
        Ok(evidence) => evidence,
        Err(error) => {
            fs::write(
                attempt.join("status"),
                format!("verification failed: {error}\n"),
            )?;
            return Err(error);
        }
    };
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    if git(&candidate, &["rev-parse", "HEAD"])? != head
        || !git(
            &candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    // Ships no longer hold `integration` while they gate; they move the
    // accepted ref under `publish`. Take it (before the claims lock, the order
    // ship uses) so the ref transaction below cannot race a ship's.
    let _publish = Lock::acquire(&state.join("publish"))?;
    if integration_head(root)? != previous {
        return Err(
            "the accepted integration moved during verification (a ship published); run integrate again"
                .into(),
        );
    }
    let _claims = Lock::acquire(&state)?;
    for (claim, source, path, bytes) in &inputs {
        if fs::read(path)? != *bytes || check_source(root, claim)? != *source {
            return Err(
                "source or reservation changed during verification; retry against its new head"
                    .into(),
            );
        }
    }
    // Sign only after the full gate, sysroot recheck, clean tree and claim checks.
    if let Some(key) = evidence
        .iter()
        .find_map(|line| line.strip_prefix(workspace_test_proof::PREFIX))
        .map(str::to_owned)
    {
        if let Ok(attestation) = sign_workspace_pass(&candidate, &key) {
            evidence.push(format!(
                "workspace-test-attestation-v1={}",
                serde_json::to_string(&attestation)?
            ));
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    let mut receipts = Vec::new();
    let native_gates = if attempt.join("native-gates.json").is_file() {
        Some(serde_json::from_slice::<serde_json::Value>(&fs::read(
            attempt.join("native-gates.json"),
        )?)?)
    } else {
        None
    };
    for (claim, source, _, bytes) in &inputs {
        let claimant = &claim.name;
        let receipt = format!(
            "susi-local-integration-v1\nagent={claimant}\nsource={source}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n",
            claim.base,
            previous.as_deref().unwrap_or("none"),
            evidence.join("\n")
        );
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!("refs/susi/integration/receipts/{claimant}/{source}");
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
        receipts.push(receipt_ref);
        if let Some(gates) = &native_gates {
            let native = native_provenance::receipt(
                root,
                native_provenance::IntegrationSource {
                    worker: claimant,
                    claim_bytes: bytes,
                    source,
                    scope_base: &scope_base(root, claim, source)?,
                    previous: previous.as_deref(),
                    accepted: &head,
                },
                gates,
            )?;
            let native_oid = native_provenance::store_blob(root, &serde_json::to_vec(&native)?)?;
            let native_ref = format!("refs/susi/provenance/receipts/{claimant}/{source}");
            transaction.push_str(&format!("create {native_ref} {native_oid}\n"));
            receipts.push(native_ref);
        }
    }
    transaction.push_str("prepare\ncommit\n");
    git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())?;
    fs::write(
        attempt.join("status"),
        format!(
            "accepted {head}\nreceipt {}\nreceipts {}\n",
            receipts.join("\nreceipt "),
            receipts.join(" ")
        ),
    )?;
    for (_, _, path, _) in &inputs {
        fs::remove_file(path)?;
    }
    // The accepted commit and receipt are durable; retain failure candidates,
    // but reclaim successful scratch builds without touching agent worktrees.
    stash_candidate_target(root, &candidate);
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "-c",
            "core.longpaths=true",
            "worktree",
            "remove",
            "--force",
            text(&candidate)?,
        ],
    ) {
        eprintln!(
            "accepted integration candidate retained at {}: {error}",
            candidate.display()
        );
    }
    println!(
        "Integrated {}: {head}; receipts {}. Primary checkout unchanged.",
        claimants.join(", "),
        receipts.join(", ")
    );
    Ok(head)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RemoteFetchState {
    NoRemoteConfigured,
    OfflineFallback { cached_main: Option<String> },
    Fetched { origin_main: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentSyncMode {
    NoRemoteConfigured,
    OfflineFallback,
    UpToDate,
    FastForwarded,
    MergedThreeWay,
}

impl AgentSyncMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoRemoteConfigured => "no_remote_configured",
            Self::OfflineFallback => "offline_fallback",
            Self::UpToDate => "up_to_date",
            Self::FastForwarded => "fast_forwarded",
            Self::MergedThreeWay => "merged_three_way",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentSyncReport {
    claimant: String,
    mode: AgentSyncMode,
    previous_head: String,
    head: String,
    upstream: Option<String>,
    claim_base: String,
}

fn has_origin_remote(root: &Path) -> bool {
    Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Offline-safe pure-Git fetch of `origin`'s `main` into `refs/remotes/origin/main`.
/// Never contacts any host REST API or `gh` CLI; falls back cleanly to local cached
/// refs when `origin` is absent or temporarily unreachable.
fn fetch_origin_optional(root: &Path, workflow_state: &Path) -> Result<RemoteFetchState> {
    if !has_origin_remote(root) {
        return Ok(RemoteFetchState::NoRemoteConfigured);
    }
    let _fetch_lock = Lock::acquire(&workflow_state.join("fetch"))?;
    let status = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=",
            "fetch",
            "--quiet",
            "--no-tags",
            "--recurse-submodules=no",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(st) if st.success() => match fetched_main(root)? {
            Some(oid) if valid_oid(&oid) => Ok(RemoteFetchState::Fetched { origin_main: oid }),
            cached_main => Ok(RemoteFetchState::OfflineFallback { cached_main }),
        },
        _ => Ok(RemoteFetchState::OfflineFallback {
            cached_main: fetched_main(root)?,
        }),
    }
}

fn can_advance_claim_base(root: &Path, target: &str) -> Result<bool> {
    match integration_head(root)? {
        Some(integrated) => Ok(integrated == target || is_ancestor(root, &integrated, target)?),
        None => Ok(true),
    }
}

fn reconcile_primary_with_origin(root: &Path, workflow_state: &Path) -> Result<RemoteFetchState> {
    let fetch_state = fetch_origin_optional(root, workflow_state)?;
    if let RemoteFetchState::Fetched { ref origin_main } = fetch_state {
        if can_advance_claim_base(root, origin_main)? {
            let local_main_ok = match Command::new("git")
                .arg("--literal-pathspecs")
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .current_dir(root)
                .args(["rev-parse", "--verify", "--quiet", "refs/heads/main"])
                .output()
            {
                Ok(out) if out.status.success() => {
                    let cur = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                    cur.is_empty() || cur == *origin_main || is_ancestor(root, &cur, origin_main)?
                }
                _ => true,
            };
            if local_main_ok {
                sync_local_main(root, origin_main);
            }
        }
    }
    Ok(fetch_state)
}

fn write_local_claim_atomic(path: &Path, claim: &Claim) -> Result<()> {
    let data = format!(
        "susi-local-claim-v2\n{}\n{}\n{}\n{}\n",
        claim.name,
        text(&claim.worktree)?,
        claim.base,
        claim.scopes.join("\n")
    );
    let temporary = path.with_extension("claim-next");
    let _ = fs::remove_file(&temporary);
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(data.as_bytes())?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
}

fn reconcile_claimed_worktree_locked(
    root: &Path,
    workflow_state: &Path,
    claimant: &str,
    fetch_state: &RemoteFetchState,
) -> Result<AgentSyncReport> {
    let claim_path = workflow_state
        .join("claims")
        .join(format!("{claimant}.claim"));
    let mut claim = read_local_claim(&claim_path)?;
    if self::state(&claim.worktree)? != workflow_state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(
            "cannot reconcile remote main into a dirty agent worktree; commit or clean first"
                .into(),
        );
    }
    let markers = [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ];
    for marker_path in git_paths(&claim.worktree, &markers)? {
        if marker_path.exists() {
            return Err("finish the existing Git operation before syncing".into());
        }
    }

    let head = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    let upstream = claim_base(root)?;
    if upstream == head || is_ancestor(root, &upstream, &head)? {
        let mode = match fetch_state {
            RemoteFetchState::NoRemoteConfigured => AgentSyncMode::NoRemoteConfigured,
            RemoteFetchState::OfflineFallback { .. } => AgentSyncMode::OfflineFallback,
            RemoteFetchState::Fetched { .. } => AgentSyncMode::UpToDate,
        };
        return Ok(AgentSyncReport {
            claimant: claimant.to_owned(),
            mode,
            previous_head: head.clone(),
            head,
            upstream: Some(upstream),
            claim_base: claim.base,
        });
    }

    if !is_ancestor(root, &claim.base, &upstream)? {
        return Err(format!(
            "upstream {upstream} does not descend from reserved claim base {}; refusing non-fast-forward reconciliation",
            claim.base
        )
        .into());
    }

    if is_ancestor(root, &head, &upstream)? {
        git(
            &claim.worktree,
            &[
                "-c",
                "core.hooksPath=",
                "merge",
                "--ff-only",
                "--quiet",
                &upstream,
            ],
        )?;
        if can_advance_claim_base(root, &upstream)? {
            claim.base = upstream.clone();
            write_local_claim_atomic(&claim_path, &claim)?;
        }
        return Ok(AgentSyncReport {
            claimant: claimant.to_owned(),
            mode: AgentSyncMode::FastForwarded,
            previous_head: head,
            head: upstream.clone(),
            upstream: Some(upstream),
            claim_base: claim.base,
        });
    }

    // Diverged: validate agent's committed scope delta first, then perform clean
    // non-conflicting 3-way merge (`git merge-tree --write-tree`) with upstream.
    check_ship_source(root, workflow_state, &claim, &upstream)?;
    let merge_out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(&claim.worktree)
        .args(["merge-tree", "--write-tree", &head, &upstream])
        .output()
        .map_err(|error| format!("git merge-tree in {}: {error}", claim.worktree.display()))?;
    if !merge_out.status.success() {
        return Err(format!(
            "upstream main ({upstream}) conflicts with local-agent/{claimant}; resolve with `git merge --no-ff --no-commit {upstream}` and `local-workflow finish-merge {claimant} {upstream} <message>`"
        )
        .into());
    }
    let merged_tree = String::from_utf8_lossy(&merge_out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if !valid_oid(&merged_tree) {
        return Err("git merge-tree returned an invalid tree oid".into());
    }
    let diff_out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(&claim.worktree)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &upstream,
            &merged_tree,
        ])
        .output()?;
    if !diff_out.status.success() {
        return Err("cannot inspect reconciled merge scope".into());
    }
    for raw_path in diff_out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let normalized = scope(std::str::from_utf8(raw_path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(
                format!("reconciled merge file is outside reserved scope: {normalized}").into(),
            );
        }
    }
    let hooks = workflow_state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    let merge_msg =
        format!("sync local-agent/{claimant} with upstream main\n\nTask: LOCAL-{claimant}\n");
    let merge_commit = git_input(
        &claim.worktree,
        &[
            "--no-replace-objects",
            "commit-tree",
            &merged_tree,
            "-p",
            &head,
            "-p",
            &upstream,
            "-F",
            "-",
        ],
        merge_msg.as_bytes(),
    )?;
    git(
        &claim.worktree,
        &[
            "-c",
            &format!("core.hooksPath={}", text(&hooks)?),
            "merge",
            "--ff-only",
            "--quiet",
            &merge_commit,
        ],
    )?;
    if can_advance_claim_base(root, &upstream)? {
        claim.base = upstream.clone();
        write_local_claim_atomic(&claim_path, &claim)?;
    }
    Ok(AgentSyncReport {
        claimant: claimant.to_owned(),
        mode: AgentSyncMode::MergedThreeWay,
        previous_head: head,
        head: merge_commit,
        upstream: Some(upstream),
        claim_base: claim.base,
    })
}

fn sync_claim(root: &Path, claimant: &str) -> Result<AgentSyncReport> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let workflow_state = state(root)?;
    let fetch_state = reconcile_primary_with_origin(root, &workflow_state)?;
    let _lock = Lock::acquire(&workflow_state)?;
    reconcile_claimed_worktree_locked(root, &workflow_state, claimant, &fetch_state)
}

fn workflow_distill(root: &Path, distill_args: &[String]) -> Result<String> {
    native_provenance::require_unmodified_history(root)?;
    let workflow_state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &workflow_state)?;
    if distill_args.first().map(String::as_str) == Some("lint") {
        let cargo_bin = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo_bin)
            .current_dir(root)
            .args([
                "run",
                "--offline",
                "--locked",
                "-p",
                "xtask",
                "--bin",
                "distill",
                "--",
            ])
            .args(distill_args)
            .output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .to_owned()
                .into());
        }
        Ok(String::from_utf8(out.stdout)?.trim_end().to_owned())
    } else {
        let mut forwarded: Vec<String> = distill_args.to_vec();
        if matches!(
            forwarded.first().map(String::as_str),
            Some("plan" | "compose" | "orchestrate")
        ) && !forwarded.iter().any(|arg| arg == "--pull")
        {
            forwarded.push("--pull".to_owned());
        }
        distill_compose::run(root, &forwarded).map_err(Into::into)
    }
}

fn start(root: &Path, claimant: &str, raw_scopes: &[String]) -> Result<PathBuf> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let mut scopes = raw_scopes
        .iter()
        .map(|s| scope(s))
        .collect::<Result<Vec<_>>>()?;
    if scopes.is_empty() {
        return Err("at least one --scope is required".into());
    }
    scopes.sort();
    scopes.dedup();
    let state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &state)?;
    let _lock = Lock::acquire(&state)?;
    for active in claims(&state)? {
        if active.name == claimant {
            return Err(format!("agent {claimant} already has a claim").into());
        }
        for requested in &scopes {
            if active.scopes.iter().any(|held| overlaps(requested, held)) {
                return Err(format!("scope {requested} overlaps agent {}", active.name).into());
            }
        }
    }
    // Agent worktrees sit beside the primary checkout, whichever worktree
    // `start` runs from (agents never run it inside the primary).
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let primary = match common.file_name() {
        Some(name) if name == ".git" => common
            .parent()
            .ok_or("git common dir has no parent")?
            .to_path_buf(),
        _ => PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?),
    };
    let base = primary
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid repository name")?;
    let worktree = primary
        .parent()
        .ok_or("repository has no parent")?
        .join(format!("{base}-agent-{claimant}"));
    let branch = format!("local-agent/{claimant}");
    let base_oid = claim_base(root)?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let worktree_text = text(&worktree)?;
    if worktree_text.chars().any(char::is_control) {
        return Err("worktree path contains control characters".into());
    }
    let claim_text = format!(
        "susi-local-claim-v2\n{claimant}\n{worktree_text}\n{base_oid}\n{}\n",
        scopes.join("\n")
    );
    if let Ok(meta) = fs::symlink_metadata(&worktree) {
        if !reusable_worktree(root, &worktree, &meta, &branch, &base_oid)? {
            return Err("destination already exists; refusing to reuse it".into());
        }
        // Reuse the released checkout so its warm target/ survives.
        fs::write(&claim_path, &claim_text)?;
        if let Err(error) = git(
            &worktree,
            &[
                "-c",
                "core.hooksPath=",
                "checkout",
                "-B",
                &branch,
                &base_oid,
            ],
        ) {
            fs::remove_file(&claim_path)?;
            return Err(error);
        }
        fs::create_dir_all(worktree.join("target"))?;
        seed_worktree_target(&primary, &worktree, true);
        return Ok(worktree);
    }
    fs::write(&claim_path, &claim_text)?;
    // Suppress legacy shell/network hooks for this local creation only. No
    // persistent Git setting changes and no remote operation are performed.
    if let Err(error) = git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_text,
            &base_oid,
        ],
    ) {
        // Retain the reservation if Git left a checkout behind, for inspection.
        if !worktree.exists() {
            fs::remove_file(claim_path)?;
        }
        return Err(error);
    }
    fs::create_dir(worktree.join("target"))?;
    seed_worktree_target(&primary, &worktree, true);
    Ok(worktree)
}

/// An existing destination is reused only when it is a real directory that
/// this repository registers as a worktree, has no uncommitted or untracked
/// work, and resetting `branch` to `base_oid` drops no commit: neither the
/// agent branch tip nor a detached HEAD may hold work outside `base_oid`.
fn reusable_worktree(
    root: &Path,
    worktree: &Path,
    meta: &fs::Metadata,
    branch: &str,
    base_oid: &str,
) -> Result<bool> {
    if meta.file_type().is_symlink() || !meta.is_dir() || !registered_worktree(root, worktree)? {
        return Ok(false);
    }
    if !git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .trim()
    .is_empty()
    {
        return Ok(false);
    }
    let branch_ref = format!("refs/heads/{branch}");
    if let Ok(tip) = git(root, &["rev-parse", "--verify", "--quiet", &branch_ref]) {
        if !tip.is_empty() && !is_ancestor(root, &tip, base_oid)? {
            return Err(
                format!("{branch} holds commits outside {base_oid}; refusing to reset it").into(),
            );
        }
    }
    if git(worktree, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
        let head = git(worktree, &["rev-parse", "HEAD"])?;
        if !is_ancestor(root, &head, base_oid)? {
            return Err("reusable worktree has a detached HEAD with unshipped commits".into());
        }
    }
    Ok(true)
}

fn registered_worktree(root: &Path, worktree: &Path) -> Result<bool> {
    let wanted = worktree.canonicalize()?;
    Ok(git(root, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| {
            Path::new(listed)
                .canonicalize()
                .is_ok_and(|listed| listed == wanted)
        }))
}

fn seed_worktree_target(primary: &Path, worktree: &Path, background: bool) {
    let Some(parent) = primary.parent() else {
        return;
    };
    if !parent.join(".susi-target-seed/.susi-seed").is_file()
        || worktree.join("target/.susi-seeded-from").is_file()
    {
        return;
    }
    // `workflow seed-target` is Susi Commercial Apps' dev workflow
    // (`susi-commercial`) since the lean-core move; an installed core `susi`
    // older than the move still carries it. The first binary that answers
    // the verb seeds; with none, the worktree keeps a cold target.
    let installed = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|local| local.join("SusiLocal/production/entry/susi.exe"))
        .filter(|path| path.is_file());
    let has_seed = |bin: &Path| {
        Command::new(bin)
            .args(["workflow", "seed-target", "--help"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let Some(seed_bin) = [
        Some(PathBuf::from("susi-commercial")),
        installed,
        Some(PathBuf::from("susi")),
    ]
    .into_iter()
    .flatten()
    .find(|bin| has_seed(bin)) else {
        return;
    };
    let mut cmd = Command::new(seed_bin);
    cmd.arg("workflow").arg("seed-target");
    if background {
        cmd.arg("--background");
    }
    cmd.arg(worktree);
    let _ = cmd.status();
}

enum ScopeChange {
    Extend,
    Shrink,
}

#[cfg(test)]
fn extend(root: &Path, claimant: &str, raw_scopes: &[String]) -> Result<()> {
    update_scopes(root, claimant, raw_scopes, ScopeChange::Extend)
}

fn update_scopes(
    root: &Path,
    claimant: &str,
    raw_scopes: &[String],
    change: ScopeChange,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    let requested = raw_scopes
        .iter()
        .map(|value| scope(value))
        .collect::<Result<Vec<_>>>()?;
    if requested.is_empty() {
        return Err("at least one --scope is required".into());
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let mut claim = read_local_claim(&path)?;
    let actual = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual.canonicalize()? != claim.worktree.canonicalize()?
        || git(root, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("scope changes must run inside the claimed agent branch and worktree".into());
    }
    if !git(root, &["status", "--porcelain"])?.is_empty() {
        return Err("scope changes require a clean agent worktree".into());
    }
    match change {
        ScopeChange::Extend => {
            for active in claims(&state)? {
                if active.name != claimant
                    && requested
                        .iter()
                        .any(|new| active.scopes.iter().any(|held| overlaps(new, held)))
                {
                    return Err(format!("requested scope overlaps agent {}", active.name).into());
                }
            }
            claim.scopes.extend(requested);
        }
        ScopeChange::Shrink => {
            if requested.iter().any(|scope| !claim.scopes.contains(scope)) {
                return Err("shrink requires an exact held scope".into());
            }
            claim.scopes.retain(|scope| !requested.contains(scope));
            if claim.scopes.is_empty() {
                return Err("use release to relinquish the last scope".into());
            }
            if git(&claim.worktree, &["rev-parse", "HEAD"])? != claim.base {
                // Validate the proposed claim against the actual committed
                // delta before atomically replacing the reservation.
                check_source(root, &claim)?;
            }
        }
    }
    claim.scopes.sort();
    claim.scopes.dedup();
    let data = format!(
        "susi-local-claim-v2\n{}\n{}\n{}\n{}\n",
        claim.name,
        text(&claim.worktree)?,
        claim.base,
        claim.scopes.join("\n")
    );
    let temporary = path.with_extension("claim-next");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(data.as_bytes())?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, path)?;
    Ok(())
}

fn release(root: &Path, claimant: &str) -> Result<()> {
    name(claimant)?;
    let state = state(root)?;
    let _ = reconcile_primary_with_origin(root, &state)?;
    let _lock = Lock::acquire(&state)?;
    let path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_local_claim(&path)?;
    if self::state(&claim.worktree)? != state {
        return Err("claim points to another repository".into());
    }
    if git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}") {
        return Err("worktree branch changed; inspect before releasing".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("worktree is dirty; preserve and commit its work before releasing".into());
    }
    let head = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if head != claim.base && !landed(root, &head)? {
        return Err(
            "committed work is neither in local integration nor on the fetched origin/main; reservation retained"
                .into(),
        );
    }
    fs::remove_file(path)?;
    println!(
        "Released {claimant}; retained branch and worktree {}",
        claim.worktree.display()
    );
    Ok(())
}

/// Collect each index and working-tree delta separately: their combined HEAD
/// diff can be empty even when the index still contains a staged change.
#[derive(Debug, Default, Eq, PartialEq)]
struct PathChanges {
    /// All staged, unstaged and untracked paths for authorization checks.
    all: Vec<String>,
    /// Paths from the worktree and untracked sets that still need `git add`.
    /// Staged paths are validated but must not be resubmitted as pathspecs:
    /// a staged deletion no longer exists, and can make `git add -A` fail.
    to_stage: Vec<String>,
}

fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>> {
    Ok(changed_path_changes(root, base)?.all)
}

fn changed_path_changes(root: &Path, base: &str) -> Result<PathChanges> {
    let mut changes = PathChanges::default();
    for (index, args) in [
        vec![
            "diff",
            "--cached",
            "--no-renames",
            "--name-only",
            "-z",
            base,
            "--",
        ],
        vec!["diff", "--no-renames", "--name-only", "-z"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ]
    .into_iter()
    .enumerate()
    {
        let out = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(root)
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(format!(
                "cannot inspect changes: {}",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        for path in out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|p| !p.is_empty())
        {
            let path = String::from_utf8(path.to_vec())?;
            changes.all.push(path.clone());
            if index != 0 {
                changes.to_stage.push(path);
            }
        }
    }
    changes.all.sort();
    changes.all.dedup();
    changes.to_stage.sort();
    changes.to_stage.dedup();
    Ok(changes)
}

fn owns_path(held: &str, changed: &str) -> bool {
    held == changed
        || changed
            .strip_prefix(held)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn claimed_changes(claim: &Claim) -> Result<Vec<String>> {
    claimed_changes_against(claim, "HEAD")
}

fn claimed_changes_against(claim: &Claim, base: &str) -> Result<Vec<String>> {
    Ok(claimed_path_changes_against(claim, base)?.all)
}

fn claimed_path_changes(claim: &Claim) -> Result<PathChanges> {
    claimed_path_changes_against(claim, "HEAD")
}

fn claimed_path_changes_against(claim: &Claim, base: &str) -> Result<PathChanges> {
    let changes = changed_path_changes(&claim.worktree, base)?;
    for path in &changes.all {
        let normalized = scope(path)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &normalized)) {
            return Err(format!(
                "changed file is outside {}'s reserved scopes: {path}",
                claim.name
            )
            .into());
        }
    }
    Ok(changes)
}

/// Scope-guarded local commit, not a test/release gate. It never changes the
/// repository's shared hook configuration or invokes its network-bound hooks.
fn commit(root: &Path, claimant: &str, message: &str, signer: Option<&str>) -> Result<()> {
    commit_with_pull(root, claimant, message, signer, signer.is_none())
}

fn commit_with_pull(
    root: &Path,
    claimant: &str,
    message: &str,
    signer: Option<&str>,
    pull_remote: bool,
) -> Result<()> {
    scoped_commit(root, claimant, message, signer, None)?;
    if pull_remote && has_origin_remote(root) {
        let _ = sync_claim(root, claimant)?;
    }
    Ok(())
}

fn scoped_commit(
    root: &Path,
    claimant: &str,
    message: &str,
    signer: Option<&str>,
    merge_parent: Option<&str>,
) -> Result<()> {
    native_provenance::require_unmodified_history(root)?;
    name(claimant)?;
    if message.trim().is_empty() || message.contains('\0') {
        return Err("commit message must be nonempty and contain no NUL".into());
    }
    if let Some(key) = signer {
        if !matches!(key.len(), 40 | 64) || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("--sign requires a complete OpenPGP fingerprint".into());
        }
    }
    let state = state(root)?;
    let _lock = Lock::acquire(&state)?;
    let claim_path = state.join("claims").join(format!("{claimant}.claim"));
    let claim = read_local_claim(&claim_path)?;
    let actual_root = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    if actual_root.canonicalize()? != claim.worktree.canonicalize()? {
        return Err("commit must run inside the claimed agent's worktree".into());
    }
    if self::state(&claim.worktree)? != state
        || git(&claim.worktree, &["branch", "--show-current"])? != format!("local-agent/{claimant}")
    {
        return Err("claim repository or branch does not match".into());
    }
    if signer.is_some() && merge_parent.is_some() {
        return Err("finish-merge does not support signing".into());
    }
    let signing = match signer {
        Some(key) => Some((key, git(&claim.worktree, &["rev-parse", "HEAD"])?)),
        None => None,
    };
    // Resolve all worktree-aware paths in one Git process, without caching.
    // Every marker remains checked here and rechecked before ref publication.
    let markers = [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ];
    let resolved = git_paths(&claim.worktree, &markers)?;
    let mut forbidden = Vec::new();
    for (marker, path) in markers.iter().zip(resolved) {
        if *marker == "MERGE_HEAD" && merge_parent.is_some() {
            continue;
        }
        if path.exists() {
            return Err("finish the existing Git operation before committing".into());
        }
        forbidden.push(path);
    }
    let changes = claimed_path_changes(&claim)?;
    if changes.all.is_empty() && merge_parent.is_none() {
        return Err("nothing to commit".into());
    }
    let hooks = state.join("empty-hooks");
    fs::create_dir_all(&hooks)?;
    if fs::symlink_metadata(&hooks)?.file_type().is_symlink()
        || fs::read_dir(&hooks)?.next().is_some()
    {
        return Err("local empty-hooks directory must be an empty real directory".into());
    }
    if let Some(expected) = merge_parent {
        return finish_scoped_merge(&claim, expected, message, &hooks, &forbidden);
    }
    // NUL-delimited literal pathspecs preserve spaces and metacharacters and
    // avoid the Windows command-line length limit for large tasks.
    if !changes.to_stage.is_empty() {
        let mut add = Command::new("git")
            .arg("--literal-pathspecs")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .current_dir(&claim.worktree)
            .args([
                "--literal-pathspecs",
                "add",
                "-A",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut input) = add.stdin.take() {
            for path in &changes.to_stage {
                input.write_all(path.as_bytes())?;
                input.write_all(&[0])?;
            }
        } else {
            return Err("could not open git add input".into());
        }
        let out = add.wait_with_output()?;
        if !out.status.success() {
            return Err(format!("git add failed: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
    }
    let hook_config = format!("core.hooksPath={}", text(&hooks)?);
    let trailer = format!("Task: LOCAL-{claimant}");
    let output = if let Some((key, parent)) = signing {
        // Sign and verify a detached commit object before publishing it. A
        // missing signer, wrong identity or concurrent branch move leaves the
        // previous head intact and the worker can retry the staged change.
        let tree = git(&claim.worktree, &["write-tree"])?;
        let claim_bytes = fs::read(&claim_path)?;
        // An optional hardware signer can wait for its operator. Unrelated
        // workers must still be able to reserve, commit and release scopes.
        drop(_lock);
        let signed = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "commit-tree",
                &tree,
                "-p",
                &parent,
                &format!("-S{key}"),
                "-m",
                message,
                "-m",
                &trailer,
            ],
        )?;
        git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "verify-commit",
                &signed,
            ],
        )?;
        let identities = git(
            &claim.worktree,
            &[
                "--no-replace-objects",
                "-c",
                "gpg.format=openpgp",
                "show",
                "-s",
                "--format=%GF%n%GP",
                &signed,
            ],
        )?;
        if !identities
            .lines()
            .any(|fingerprint| fingerprint.eq_ignore_ascii_case(key))
        {
            return Err("signed commit did not verify under the requested fingerprint".into());
        }
        let _publish = Lock::acquire(&state)?;
        if fs::read(&claim_path)? != claim_bytes
            || git(&claim.worktree, &["branch", "--show-current"])?
                != format!("local-agent/{claimant}")
            || git(&claim.worktree, &["write-tree"])? != tree
        {
            return Err("worker reservation, branch or staged tree changed while signing".into());
        }
        claimed_changes(&claim)?;
        git(
            &claim.worktree,
            &[
                "-c",
                &hook_config,
                "update-ref",
                &format!("refs/heads/local-agent/{claimant}"),
                &signed,
                &parent,
            ],
        )?;
        format!("Verified OpenPGP commit {signed} with fingerprint {key}.")
    } else {
        git(
            &claim.worktree,
            &[
                "-c",
                &hook_config,
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                message,
                "-m",
                &trailer,
            ],
        )?
    };
    println!(
        "{output}\nLocal scope guard passed; this commit is not a test or release attestation."
    );
    Ok(())
}

/// Finish only a resolved, explicitly pinned merge in the reserved worktree.
/// ORIG_HEAD lets a retry finish cleanup if publication already succeeded.
fn finish_scoped_merge(
    claim: &Claim,
    expected: &str,
    message: &str,
    hooks: &Path,
    forbidden: &[PathBuf],
) -> Result<()> {
    let root = &claim.worktree;
    let claim_path = state(root)?
        .join("claims")
        .join(format!("{}.claim", claim.name));
    if !matches!(expected.len(), 40 | 64) || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("finish-merge requires a full commit object ID".into());
    }
    let object = git(
        root,
        &["rev-parse", "--verify", &format!("{expected}^{{commit}}")],
    )?;
    if object != expected {
        return Err("expected parent must name the commit itself".into());
    }
    let original = git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])?;
    if original == expected {
        return Err("merge parents must be distinct".into());
    }
    git(
        root,
        &["merge-base", "--is-ancestor", &claim.base, &original],
    )?;
    let merge_head = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD",
        ],
    )?;
    let tree = git(root, &["write-tree"])?;
    let branch = format!("local-agent/{}", claim.name);
    let validate = || -> Result<()> {
        if read_claim(&claim_path)? != *claim
            || forbidden.iter().any(|p| p.exists())
            || fs::read_to_string(&merge_head)?.lines().collect::<Vec<_>>() != [expected]
            || git(root, &["branch", "--show-current"])? != branch
            || git(root, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])? != original
            || git(root, &["write-tree"])? != tree
        {
            return Err("merge parent, branch or staged tree changed".into());
        }
        for args in [
            &["ls-files", "--unmerged", "-z"][..],
            &["diff", "--name-only", "-z", "--"][..],
            &["ls-files", "--others", "--exclude-standard", "-z"][..],
        ] {
            if !git(root, args)?.is_empty() {
                return Err(
                    "finish-merge requires a resolved index and fully staged changes".into(),
                );
            }
        }
        claimed_changes_against(claim, &original)?;
        Ok(())
    };
    validate()?;
    let message = format!("{}\n\nTask: LOCAL-{}", message.trim(), claim.name);
    let expected_shape = format!("{tree}\n{original} {expected}\n{message}");
    let head = git(root, &["rev-parse", "HEAD"])?;
    let commit = if head == original {
        git_input(
            root,
            &[
                "--no-replace-objects",
                "commit-tree",
                &tree,
                "-p",
                &original,
                "-p",
                expected,
                "-F",
                "-",
            ],
            message.as_bytes(),
        )?
    } else {
        head.clone()
    };
    if git(
        root,
        &[
            "--no-replace-objects",
            "show",
            "-s",
            "--format=%T%n%P%n%B",
            &commit,
        ],
    )? != expected_shape
    {
        return Err("refusing a merge with different parents, tree or task message".into());
    }
    let hooks = format!("core.hooksPath={}", text(hooks)?);
    validate()?;
    if head == original {
        git(
            root,
            &[
                "-c",
                &hooks,
                "update-ref",
                &format!("refs/heads/{branch}"),
                &commit,
                &original,
            ],
        )?;
    }
    validate()?;
    if git(root, &["rev-parse", "HEAD"])? != commit {
        return Err("claimed branch moved before merge cleanup".into());
    }
    git(root, &["-c", &hooks, "merge", "--quit"])?;
    println!("Finished scoped merge {commit}, parents {original} {expected}, tree {tree}.");
    Ok(())
}

fn require_ship_ref(git_ref: &str) -> Result<()> {
    if git_ref != "refs/heads/main" {
        return Err("ship pushes only verified fast-forwards to refs/heads/main".into());
    }
    Ok(())
}

/// The git remote a ship fetches `main` from and fast-forwards. Plain git
/// only: GitHub is storage (CEO mandate 2026-11-09), so a landing posts
/// nothing to the host; its receipt commit is the record.
trait Remote {
    fn fetch_main(&mut self, root: &Path) -> Result<String>;
    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()>;
}

struct GitRemote;

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(character);
    }
    out
}

fn sync_local_main(root: &Path, sha: &str) {
    let _ = git(root, &["update-ref", "refs/remotes/origin/main", sha]);
    if let Ok(common) = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        if let Some(primary) = Path::new(&common).parent() {
            if git(primary, &["branch", "--show-current"]).ok().as_deref() == Some("main") {
                // Moving a checked-out branch ref without its files would make
                // the checkout look like it reverts the shipped work. Untracked
                // files are fine: a fast-forward refuses to overwrite them.
                let clean = git(primary, &["status", "--porcelain", "--untracked-files=no"])
                    .is_ok_and(|status| status.is_empty());
                let merged = clean
                    && git(
                        primary,
                        &[
                            "-c",
                            "core.hooksPath=",
                            "merge",
                            "--ff-only",
                            "--quiet",
                            sha,
                        ],
                    )
                    .is_ok();
                if !merged {
                    eprintln!(
                        "local main in {} was not fast-forwarded (uncommitted changes); run `git merge --ff-only origin/main` there",
                        primary.display()
                    );
                }
                return;
            }
        }
    }
    let checked_out = git(root, &["worktree", "list", "--porcelain"])
        .is_ok_and(|list| list.lines().any(|line| line == "branch refs/heads/main"));
    if !checked_out {
        let _ = git(root, &["update-ref", "refs/heads/main", sha]);
    }
}

impl Remote for GitRemote {
    fn fetch_main(&mut self, root: &Path) -> Result<String> {
        git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "fetch",
                "--quiet",
                "--no-tags",
                "--recurse-submodules=no",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )?;
        let oid = git(root, &["rev-parse", "origin/main"])?;
        if !valid_oid(&oid) {
            return Err("origin/main is not a commit oid".into());
        }
        Ok(oid)
    }

    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()> {
        require_ship_ref(git_ref)?;
        if !valid_oid(sha) {
            return Err("ship push requires a commit oid".into());
        }
        git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                "origin",
                &format!("{sha}:{git_ref}"),
            ],
        )?;
        sync_local_main(root, sha);
        Ok(())
    }
}

/// What a ship published: `head` is the commit now on origin/main (the
/// receipt commit), `merge` the verified integration head it sits on and
/// whose receipt it carries.
#[derive(Debug)]
struct ShipReport {
    head: String,
    merge: String,
}

fn gate_evidence<'a>(evidence: &'a [String], subcommand: &str) -> Result<&'a str> {
    evidence
        .iter()
        .find(|line| {
            let Some((command, status)) = line.split_once(" => ") else {
                return false;
            };
            status.starts_with("exit code: 0;")
                && command.split_whitespace().nth(1) == Some(subcommand)
        })
        .map(String::as_str)
        .ok_or_else(|| format!("missing passing {subcommand} evidence").into())
}

fn require_ship_evidence(evidence: &[String]) -> Result<()> {
    if evidence.is_empty() {
        return Err("verification returned no evidence".into());
    }
    // Gate lines are `cargo ... => exit code: N; ...`. Other evidence (sysroot,
    // test proof, gate selection) describes the run and carries no status.
    for line in evidence.iter().filter(|line| line.starts_with("cargo ")) {
        match line.split_once(" => ") {
            Some((_, status)) if status.starts_with("exit code: 0;") => {}
            _ => return Err(format!("refusing to publish a failed gate: {line}").into()),
        }
    }
    for subcommand in ["fmt", "deny", "clippy"] {
        gate_evidence(evidence, subcommand)?;
    }
    if gate_evidence(evidence, "nextest").is_err() {
        gate_evidence(evidence, "test")?;
    }
    Ok(())
}

struct SourceInput {
    claim: Claim,
    source: String,
    bytes: Vec<u8>,
    path: PathBuf,
}

fn ship_scope_base(root: &Path, claim: &Claim, source: &str, publish_base: &str) -> Result<String> {
    let base = match scope_base(root, claim, source) {
        Ok(base) => base,
        Err(error) => {
            let on_publish_base =
                claim.base == publish_base || is_ancestor(root, &claim.base, publish_base)?;
            // Falling back to the claim base exempts nothing: the whole
            // claim.base..source diff stays subject to the claimed scopes.
            let message = error.to_string();
            let recoverable = message.contains("accepted history does not descend")
                || message.contains("accepted integration has no matching durable");
            if recoverable && is_ancestor(root, &claim.base, source)? && on_publish_base {
                claim.base.clone()
            } else {
                return Err(error);
            }
        }
    };
    // History the source shares with the publish base is already on
    // origin/main: ship merges onto it and gates the result, so it is not
    // this worker's change to answer for. When origin/main advanced past the
    // accepted integration without a receipt (a push around the gate, seen
    // 2026-11-07: 170 commits) and the agent merged it, measuring from the
    // accepted integration would blame every one of those files on the
    // agent. The scope base moves up to the shared ancestor, never past
    // anything the agent added itself.
    let shared = git(root, &["merge-base", "--all", source, publish_base])?;
    let shared: Vec<_> = shared.lines().collect();
    if let [shared] = shared.as_slice() {
        if *shared != base && is_ancestor(root, &base, shared)? {
            return Ok((*shared).to_owned());
        }
    }
    Ok(base)
}

/// The expected directory belongs to the lock held by this operation. Recheck
/// at each batch boundary; never carry this observation across a gate silently.
fn require_workflow(root: &Path, expected: &Path) -> Result<()> {
    if state(root)? != expected {
        return Err("repository workflow directory changed during operation".into());
    }
    Ok(())
}

fn check_ship_source(
    root: &Path,
    workflow: &Path,
    claim: &Claim,
    publish_base: &str,
) -> Result<String> {
    if state(&claim.worktree)? != workflow
        || git(&claim.worktree, &["branch", "--show-current"])?
            != format!("local-agent/{}", claim.name)
    {
        return Err("claim repository or branch does not match".into());
    }
    if !git(
        &claim.worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("integration requires a clean committed agent worktree".into());
    }
    let source = git(&claim.worktree, &["rev-parse", "HEAD"])?;
    if !is_ancestor(root, &claim.base, &source)? {
        return Err("source no longer descends from its reserved base".into());
    }
    let scope_base = ship_scope_base(root, claim, &source, publish_base)?;
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .current_dir(root)
        .args([
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &scope_base,
            &source,
        ])
        .output()
        .map_err(|error| format!("git diff in {}: {error}", root.display()))?;
    if !out.status.success() {
        return Err("cannot inspect committed scope".into());
    }
    for path in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = scope(std::str::from_utf8(path)?)?;
        if !claim.scopes.iter().any(|held| owns_path(held, &path)) {
            return Err(format!("committed file is outside reserved scope: {path}").into());
        }
    }
    Ok(source)
}

fn load_sources(
    root: &Path,
    workflow: &Path,
    claimants: &[&str],
    publish_base: &str,
) -> Result<Vec<SourceInput>> {
    if claimants.is_empty() {
        return Err("at least one claimant is required".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for claimant in claimants {
        name(claimant)?;
        if !unique.insert(*claimant) {
            return Err("duplicate integration claimant".into());
        }
    }
    require_workflow(root, workflow)?;
    let _claims = Lock::acquire(workflow)?;
    let inputs = claimants
        .iter()
        .map(|claimant| {
            let path = workflow.join("claims").join(format!("{claimant}.claim"));
            let bytes = fs::read(&path)?;
            let claim = read_local_claim(&path)?;
            let source = check_ship_source(root, workflow, &claim, publish_base)?;
            Ok(SourceInput {
                claim,
                source,
                bytes,
                path,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    require_workflow(root, workflow)?;
    Ok(inputs)
}

/// One stable candidate worktree per repository, beside its primary checkout
/// (`<repo>-verify-candidate`). Sibling repositories never share it, and the
/// warm integration target keeps the same paths from run to run.
fn candidate_path(root: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let primary = common.parent().ok_or("git common dir has no parent")?;
    let name = primary
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("primary checkout has no name")?;
    Ok(primary
        .parent()
        .ok_or("primary checkout has no parent")?
        .join(format!("{name}-verify-candidate")))
}

/// Where `candidate`'s warm target rests between runs, in order of
/// preference. A slot candidate (`<repo>-verify-candidate-<n>`) has its own
/// `slots/<n>/target`; when that does not exist yet it adopts the single
/// legacy `integration-target` (registry dependencies stay fresh; workspace
/// crates rebuild once for the new path). The legacy candidate
/// (`integrate`) uses only the legacy target.
fn warm_targets(workflow: &Path, candidate: &Path) -> Vec<PathBuf> {
    let legacy = workflow.join("integration-target");
    let slot = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once("-verify-candidate-"))
        .map(|(_, slot)| slot)
        .filter(|slot| !slot.is_empty() && slot.bytes().all(|byte| byte.is_ascii_digit()));
    match slot {
        Some(slot) => vec![workflow.join("slots").join(slot).join("target"), legacy],
        None => vec![legacy],
    }
}

/// Move a warm target into `candidate` (a rename: atomic, so two candidates
/// never take the same one) and return the candidate's target directory.
fn restore_candidate_target(candidate: &Path) -> PathBuf {
    let target = candidate.join("target");
    if !target.exists() {
        if let Ok(workflow) = state(candidate) {
            for warm in warm_targets(&workflow, candidate) {
                if warm.is_dir() && fs::rename(&warm, &target).is_ok() {
                    break;
                }
            }
        }
    }
    target
}

fn stash_candidate_target(root: &Path, candidate: &Path) {
    let target = candidate.join("target");
    if target.is_dir() {
        if let Ok(workflow) = state(root) {
            if let Some(warm) = warm_targets(&workflow, candidate).into_iter().next() {
                if !warm.exists() {
                    if let Some(parent) = warm.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&target, &warm);
                }
            }
        }
    }
}

/// Remove a previous run's `candidate` worktree, or say exactly why it
/// cannot be removed. On Windows a target directory whose files a leftover
/// process still holds open is only partly deleted; `worktree add` into the
/// remains then fails with a misleading error, so the real removal error is
/// returned instead of ignored.
fn clear_candidate(root: &Path, candidate: &Path) -> Result<()> {
    if candidate.exists() {
        remove_candidate(root, candidate);
    }
    if candidate.exists() {
        if let Err(error) = fs::remove_dir_all(candidate) {
            if candidate.exists() {
                return Err(format!(
                    "cannot remove the previous candidate {}: {error}",
                    candidate.display()
                )
                .into());
            }
        }
    }
    // Only this candidate's record: a bare `git worktree prune` also deletes
    // the records of live worktrees registered from the Windows boot.
    worktree_admin::forget(root, candidate)?;
    Ok(())
}

fn remove_candidate(root: &Path, candidate: &Path) {
    stash_candidate_target(root, candidate);
    if let Ok(path) = text(candidate) {
        let _ = git(
            root,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "core.longpaths=true",
                "worktree",
                "remove",
                "--force",
                path,
            ],
        );
    }
}

/// Recreate the slot's `candidate` worktree at `base` and merge every source
/// into it, in order. Both check files out as committed
/// (`gate_receipt::CHECKOUT_AS_COMMITTED`), so the gate tests the bytes its
/// receipt names whatever the host's `core.autocrlf` says.
fn merge_onto(root: &Path, candidate: &Path, base: &str, inputs: &[SourceInput]) -> Result<()> {
    if !valid_oid(base) {
        return Err("ship base is not a commit oid".into());
    }
    clear_candidate(root, candidate)?;
    let [c1, autocrlf, c2, eol] = gate_receipt::CHECKOUT_AS_COMMITTED;
    git(
        root,
        &[
            "-c",
            "core.hooksPath=",
            c1,
            autocrlf,
            c2,
            eol,
            "worktree",
            "add",
            "--detach",
            text(candidate)?,
            base,
        ],
    )?;
    for input in inputs {
        if let Err(error) = git(
            candidate,
            &[
                "-c",
                "core.hooksPath=",
                c1,
                autocrlf,
                c2,
                eol,
                "-c",
                "user.name=Susi Local Integration",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "merge",
                "--no-ff",
                "--no-edit",
                &input.source,
            ],
        ) {
            remove_candidate(root, candidate);
            return Err(error);
        }
    }
    Ok(())
}

struct PublicationHead<'a> {
    head: &'a str,
    observed_base: &'a str,
}

fn record_acceptance(
    root: &Path,
    attempt: &Path,
    inputs: &[SourceInput],
    publication: PublicationHead<'_>,
    evidence: &[String],
) -> Result<()> {
    let PublicationHead {
        head,
        observed_base: publish_base,
    } = publication;
    // This is the exact final fetch checked under the publication lock, not
    // HEAD^1: after a multi-source batch that parent is an intermediate merge.
    if !valid_oid(publish_base) || !is_ancestor(root, publish_base, head)? {
        return Err("fetched publication base is not contained in the verified head".into());
    }
    let native_gates = if attempt.join("native-gates.json").is_file() {
        Some(serde_json::from_slice::<serde_json::Value>(&fs::read(
            attempt.join("native-gates.json"),
        )?)?)
    } else {
        None
    };
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let previous = integration_head(root)?;
    // The receipt's `previous` names the accepted history this integration
    // builds on. A displaced unrelated ref is not that history: record the
    // publish base instead, or the receipt can never verify and every later
    // ship fails at load. The ref transaction still compares against the
    // actual old value.
    let mut chain_previous = previous.clone();
    if let Some(previous) = &previous {
        if !is_ancestor(root, previous, head)? {
            git(
                root,
                &["update-ref", "refs/susi/integration/displaced", previous],
            )?;
            eprintln!(
                "preserved unrelated integration ref {previous} at refs/susi/integration/displaced"
            );
            chain_previous = Some(publish_base.to_owned());
        } else if previous.as_str() != publish_base && is_ancestor(root, previous, publish_base)? {
            // The local ref is behind origin/main (main advanced through ships
            // made elsewhere). The history this integration builds on is the
            // publish base it was merged onto, not the stale ref, which can be
            // older than the claim base and would make the receipt unverifiable.
            chain_previous = Some(publish_base.to_owned());
        }
    }
    let zero = "0".repeat(head.len());
    let mut transaction = format!(
        "start\nupdate {INTEGRATION_REF} {head} {}\n",
        previous.as_deref().unwrap_or(&zero)
    );
    for input in inputs {
        let receipt = format!(
            "susi-local-integration-v1\nagent={}\nsource={}\nclaim-base={}\nprevious={}\ncommit={head}\ntree={tree}\n{}\n",
            input.claim.name,
            input.source,
            input.claim.base,
            chain_previous.as_deref().unwrap_or("none"),
            evidence.join("\n")
        );
        let oid = git_input(root, &["hash-object", "-w", "--stdin"], receipt.as_bytes())?;
        let receipt_ref = format!(
            "refs/susi/integration/receipts/{}/{}",
            input.claim.name, input.source
        );
        transaction.push_str(&format!("create {receipt_ref} {oid}\n"));
        if let Some(gates) = &native_gates {
            let scope_base = timed("acceptance: scope base", || {
                ship_scope_base(root, &input.claim, &input.source, publish_base)
            })?;
            let native = timed("acceptance: provenance receipt", || {
                native_provenance::receipt(
                    root,
                    native_provenance::IntegrationSource {
                        worker: &input.claim.name,
                        claim_bytes: &input.bytes,
                        source: &input.source,
                        scope_base: &scope_base,
                        previous: chain_previous.as_deref(),
                        accepted: head,
                    },
                    gates,
                )
            })?;
            let native_oid = timed("acceptance: store receipt", || {
                native_provenance::store_blob(root, &serde_json::to_vec(&native)?)
            })?;
            let native_ref = format!(
                "refs/susi/provenance/receipts/{}/{}",
                input.claim.name, input.source
            );
            transaction.push_str(&format!("create {native_ref} {native_oid}\n"));
        }
    }
    // The public gate receipt: `.agents/receipts/<head>.json` in a
    // receipt-only commit on top of the verified head, recorded in the same
    // ref transaction. `publish_head` pushes that commit; without it nothing
    // is published.
    let sources: Vec<serde_json::Value> = inputs
        .iter()
        .map(|input| {
            let task = native_provenance::task_for_claim(
                root,
                &input.source,
                &input.claim.name,
                &input.claim.scopes,
            )
            .ok()
            .map(|task| task.id);
            serde_json::json!({
                "agent": input.claim.name,
                "commit": input.source,
                "claim_base": input.claim.base,
                "task": task,
            })
        })
        .collect();
    let receipt = timed("acceptance: gate receipt", || {
        gate_receipt::build(&gate_receipt::Landing {
            root,
            head,
            origin_main: publish_base,
            sources: sources.clone(),
            native_gates: native_gates.as_ref(),
            evidence,
        })
    })
    .map_err(|error| format!("gate receipt: {error}"))?;
    let receipt_commit = timed("acceptance: receipt commit", || {
        gate_receipt::commit_receipt(
            root,
            head,
            &receipt,
            &gate_receipt::commit_message(head, &sources),
            &attempt.join("receipt-index"),
        )
    })
    .map_err(|error| format!("gate receipt commit: {error}"))?;
    transaction.push_str(&format!(
        "update {} {receipt_commit}\n",
        gate_receipt::ref_for(head)
    ));
    transaction.push_str("prepare\ncommit\n");
    timed("acceptance: ref transaction", || {
        git_input(root, &["update-ref", "--stdin"], transaction.as_bytes())
    })?;
    for input in inputs {
        fs::remove_file(&input.path)?;
    }
    Ok(())
}

/// Run `verify` on the slot's merged candidate. `selection_base` is what the
/// gate's affected selection is computed against: the publish base on a first
/// run, the previously verified head on a re-gate.
fn verify_merged<F>(
    slot: &Slot,
    selection_base: &str,
    inputs: &[SourceInput],
    verify: &mut F,
) -> Result<(String, Vec<String>)>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let candidate = &slot.candidate;
    let attempt = &slot.attempt;
    fs::create_dir_all(attempt)?;
    // The native provenance receipt carries the final run's gates; an earlier
    // run of this ship (before a re-gate) stays beside it for inspection.
    if attempt.join("native-gates.json").is_file() {
        let selection = fs::read_to_string(attempt.join("selection-base")).unwrap_or_default();
        let _ = fs::rename(
            attempt.join("native-gates.json"),
            attempt.join(format!("native-gates.{}.json", selection.trim())),
        );
    }
    fs::write(
        attempt.join("selection-base"),
        format!("{selection_base}\n"),
    )?;
    fs::write(
        attempt.join("native-inputs.json"),
        serde_json::to_vec_pretty(
            &inputs
                .iter()
                .map(|input| {
                    serde_json::json!({
                        "worker": input.claim.name,
                        "source": input.source,
                        "scopes": input.claim.scopes,
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    let before = git(candidate, &["rev-parse", "HEAD"])?;
    let evidence = verify(candidate, attempt)?;
    let head = git(candidate, &["rev-parse", "HEAD"])?;
    if head != before
        || !git(
            candidate,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        return Err("verification changed the candidate tree; refusing promotion".into());
    }
    require_ship_evidence(&evidence)?;
    Ok((head, evidence))
}

/// Whether the re-gate after merging newer origin/main commits must run, and
/// a description of what it covers. The gates already passed on `tested`. A
/// package whose own files and dependencies the newly merged commits leave
/// unchanged builds and tests the same on the new head, so the re-gate's
/// selection is the change from `tested` to the candidate's new `HEAD`
/// (`affected.rs`; the root package's tree-wide tests are in every such
/// selection). When the merged tree equals the tested tree, or the change
/// selects no package, the gates already run cover the new head as is.
fn regate_needed(candidate: &Path, tested: &str) -> (bool, String) {
    if env::var("SUSI_LOCAL_GATE").is_ok_and(|mode| mode == "full") {
        return (true, "SUSI_LOCAL_GATE=full".into());
    }
    let selection = affected::changed(candidate, tested, "HEAD").and_then(|changed| {
        let workspace = affected::load(candidate)?;
        Ok((changed.len(), affected::select(&workspace, &changed)))
    });
    match selection {
        Ok((0, _)) => (false, "the merged tree equals the tested tree".into()),
        Ok((changed, affected::Selection::Packages(packages))) if packages.is_empty() => {
            (false, format!("{changed} changed paths select no package"))
        }
        Ok((changed, affected::Selection::Packages(packages))) => (
            true,
            format!(
                "{changed} changed paths select {} packages: {}",
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        ),
        Ok((changed, affected::Selection::Workspace(reason))) => (
            true,
            format!("{changed} changed paths select the whole workspace ({reason})"),
        ),
        Err(error) => (true, format!("selection unavailable ({error})")),
    }
}

/// How many times a ship re-merges onto a moved origin/main and re-gates
/// before it gives up (nothing published) and asks to be run again.
const SHIP_ROUNDS: usize = 5;

/// Verify and publish `claimants` to origin/main.
///
/// The gate runs in a private slot (`acquire_slot`) without any host or
/// repository queue, so concurrent ships gate side by side. Only publication
/// is serialized, under the repository's `publish` lock: re-fetch origin/main,
/// re-check the sources and reservations, record the receipts and move
/// `refs/susi/integration/current` in one ref transaction, push (a
/// fast-forward, never forced) and post statuses. If origin/main moved while
/// the gate ran, the lock is released, the sources are merged onto the new
/// origin/main and only what the newly merged commits affect is re-gated
/// (`regate_needed`); then publication is tried again.
fn ship<F>(
    root: &Path,
    claimants: &[&str],
    remote: &mut dyn Remote,
    mut verify: F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let slot = timed("take verification slot", || acquire_slot(root))
        .map_err(|error| format!("slot: {error}"))?;
    eprintln!(
        "ship: verifying in {} (no queue; other ships gate at the same time)",
        slot.candidate.display()
    );
    // Native gate records of the slot's previous ship are not this ship's.
    if let Ok(entries) = fs::read_dir(&slot.attempt) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("native-gates") && name.ends_with(".json") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let result = ship_in_slot(root, &slot, claimants, remote, &mut verify);
    timed("stash warm target", || {
        remove_candidate(root, &slot.candidate)
    });
    result
}

/// A workspace proof binds commit identity, not just the tree. Retain old
/// evidence for audit, but never expose its machine-readable key after remerge.
fn retire_workspace_proofs(evidence: &mut [String], tested: &str, merged: &str) {
    if tested == merged {
        return;
    }
    for line in evidence {
        if line.starts_with(workspace_test_proof::PREFIX)
            || line.starts_with("workspace-test-attestation-v1=")
        {
            *line = format!("historical workspace proof head={tested}: {line}");
        }
    }
}
fn ship_in_slot<F>(
    root: &Path,
    slot: &Slot,
    claimants: &[&str],
    remote: &mut dyn Remote,
    verify: &mut F,
) -> Result<ShipReport>
where
    F: FnMut(&Path, &Path) -> Result<Vec<String>>,
{
    let workflow = state(root).map_err(|error| format!("state: {error}"))?;
    let workflow = workflow.as_path();
    // Fetches update `refs/remotes/origin/main`; two at once can collide on
    // its ref lock. The first fetch takes a lock of its own (seconds), not
    // `publish`: a launcher built before slots holds `publish` for its whole
    // gate, and a new ship must not wait for that before its own gate. The
    // later fetches run under `publish`.
    let mut base = {
        let _fetch = timed("wait for fetch lock", || {
            Lock::acquire(&workflow.join("fetch"))
        })
        .map_err(|error| format!("lock: {error}"))?;
        timed("fetch origin/main", || remote.fetch_main(root))
            .map_err(|error| format!("fetch: {error}"))?
    };
    let inputs = timed("load sources", || {
        load_sources(root, workflow, claimants, &base)
    })
    .map_err(|error| format!("load: {error}"))?;
    timed("merge candidate", || {
        merge_onto(root, &slot.candidate, &base, &inputs)
    })
    .map_err(|error| format!("merge: {error}"))?;
    let (mut head, mut evidence) =
        verify_merged(slot, &base, &inputs, verify).map_err(|error| format!("prove: {error}"))?;
    for round in 1..=SHIP_ROUNDS {
        let publish = timed("wait for publish lock", || {
            Lock::acquire(&workflow.join("publish"))
        })
        .map_err(|error| format!("lock: {error}"))?;
        timed("wait for legacy integration", || {
            wait_for_legacy_integration(workflow);
        });
        let current = {
            let _fetch = Lock::acquire(&workflow.join("fetch"))?;
            timed("fetch origin/main", || remote.fetch_main(root))?
        };
        if current == base {
            {
                let _claims = Lock::acquire(workflow)?;
                require_workflow(root, workflow)?;
                for input in &inputs {
                    if fs::read(&input.path)? != input.bytes
                        || check_ship_source(root, workflow, &input.claim, &base)? != input.source
                    {
                        return Err(
                            "source or reservation changed during verification; retry against its new head"
                                .into(),
                        );
                    }
                }
                require_workflow(root, workflow)?;
                timed("record acceptance", || {
                    record_acceptance(
                        root,
                        &slot.attempt,
                        &inputs,
                        PublicationHead {
                            head: &head,
                            observed_base: &base,
                        },
                        &evidence,
                    )
                })?;
            }
            let report = timed("publish", || publish_head(root, &head, &evidence, remote));
            drop(publish);
            return report;
        }
        drop(publish);
        if !is_ancestor(root, &base, &current)? {
            return Err(format!(
                "origin/main moved from {base} to {current}, which does not contain it; nothing published"
            )
            .into());
        }
        if round == SHIP_ROUNDS {
            break;
        }
        let tested = head.clone();
        eprintln!(
            "ship: origin/main moved from {base} to {current} during the gate; merging onto it and re-gating what the new commits affect"
        );
        timed("re-merge candidate", || {
            merge_onto(root, &slot.candidate, &current, &inputs)
        })
        .map_err(|error| format!("re-merge: {error}"))?;
        let merged = git(&slot.candidate, &["rev-parse", "HEAD"])?;
        retire_workspace_proofs(&mut evidence, &tested, &merged);
        let (needed, covers) = timed("re-gate selection", || {
            regate_needed(&slot.candidate, &tested)
        });
        // One evidence line, never shaped like a gate status or a receipt
        // key: `covers` can carry cargo's error text.
        let covers: String = covers
            .replace(" => ", " -> ")
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        evidence.push(format!(
            "regate base={current} tested={tested} head={merged} {}: {covers}",
            if needed { "ran" } else { "skipped" }
        ));
        if needed {
            let (regated, more) = verify_merged(slot, &tested, &inputs, verify)
                .map_err(|error| format!("re-gate: {error}"))?;
            head = regated;
            evidence.extend(more);
        } else {
            head = merged;
        }
        base = current;
    }
    Err(format!(
        "origin/main moved during each of {SHIP_ROUNDS} gate rounds; nothing published; run ship again"
    )
    .into())
}

fn ship_retry(root: &Path, remote: &mut dyn Remote) -> Result<ShipReport> {
    let workflow = state(root)?;
    let _publish = Lock::acquire(&workflow.join("publish"))?;
    let Some(head) = integration_head(root)? else {
        return Err("missing receipt".into());
    };
    let tree = git(root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let receipt = matching_receipt(root, &head, &tree)?.ok_or("missing receipt")?;
    let evidence: Vec<String> = receipt
        .lines()
        .skip(7)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    require_ship_evidence(&evidence)?;
    let base = remote.fetch_main(root)?;
    if !is_ancestor(root, &base, &head)? {
        return Err("integration head is not based on origin/main".into());
    }
    let again = remote.fetch_main(root)?;
    if again != base {
        return Err("origin/main moved twice".into());
    }
    publish_head(root, &head, &evidence, remote)
}

fn publish_head(
    root: &Path,
    head: &str,
    evidence: &[String],
    remote: &mut dyn Remote,
) -> Result<ShipReport> {
    // What reaches origin/main is the receipt commit: the verified head plus
    // `.agents/receipts/<head>.json`. No receipt, no publication.
    let published = gate_receipt::receipt_commit(root, head)
        .map_err(|error| format!("gate receipt for {head}: {error}; nothing published"))?
        .ok_or_else(|| {
            format!(
                "no gate receipt commit recorded at {} for verified head {head}; this head was accepted without a receipt (older ship tool), run ship again; nothing published",
                gate_receipt::ref_for(head)
            )
        })?;
    // Every gate passed on the head being published (checked again here, before
    // the push, where the status posts used to read the same evidence after it).
    require_ship_evidence(evidence)?;
    remote.push_ref(root, &published, "refs/heads/main")?;
    Ok(ShipReport {
        head: published,
        merge: head.to_owned(),
    })
}

fn print_ship(report: &ShipReport) {
    println!(
        "Shipped {} directly to origin/main: verified merge {}, gate receipt {} (verify with `cargo xtask verify-receipt {}`).",
        report.head,
        report.merge,
        gate_receipt::path_for(&report.merge),
        report.merge
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepoKind {
    Susi,
    Commercial,
}

fn repo_kind(root: &Path) -> Result<RepoKind> {
    if root.join("crates/susi-core/Cargo.toml").is_file() {
        return Ok(RepoKind::Susi);
    }
    let manifest = root.join("Cargo.toml");
    if manifest.is_file() {
        let text = fs::read_to_string(&manifest)?;
        if text
            .lines()
            .any(|line| line.trim() == "name = \"susi-commercial\"")
        {
            return Ok(RepoKind::Commercial);
        }
    }
    Err("unrecognized checkout".into())
}

fn commercial_gate_argv() -> Vec<Vec<String>> {
    vec![
        vec!["fmt".into(), "--all".into(), "--check".into()],
        vec![
            "clippy".into(),
            "--workspace".into(),
            "--all-targets".into(),
            "--locked".into(),
            "--".into(),
            "-D".into(),
            "warnings".into(),
        ],
        vec!["test".into(), "--workspace".into(), "--locked".into()],
        vec![
            "deny".into(),
            "check".into(),
            "licenses".into(),
            "advisories".into(),
            "bans".into(),
            "sources".into(),
        ],
    ]
}

fn commercial_gate(candidate: &Path, attempt: &Path) -> Result<Vec<String>> {
    let _live = live_gate();
    let compiler = Command::new("rustc")
        .current_dir(candidate)
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc in {}: {error}", candidate.display()))?;
    if !compiler.status.success() {
        return Err("cannot identify native compiler".into());
    }
    let compiler = String::from_utf8(compiler.stdout)?;
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("compiler did not report host")?;
    if cfg!(windows) && !host.ends_with("-pc-windows-msvc") {
        return Err("Windows integration requires a native MSVC compiler".into());
    }
    let original_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("cannot identify toolchain home")?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let home = attempt.join("test-home");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    let target = restore_candidate_target(candidate);
    let mut gates = commercial_gate_argv();
    let selection = gate_selection(candidate, attempt).map(|(selection, _)| selection);
    let whole_workspace = !matches!(selection, Ok(affected::Selection::Packages(_)));
    if let Ok(affected::Selection::Packages(packages)) = &selection {
        for args in &mut gates {
            affected::narrow(args, packages);
        }
    }
    require_gate_disk(candidate, &target, whole_workspace)?;
    let mut evidence = Vec::new();
    for args in gates {
        let started = std::time::Instant::now();
        let mut command = Command::new("cargo");
        command.current_dir(candidate).args(&args);
        for (key, _) in env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("SUSI_")
                || [
                    "TOKEN",
                    "SECRET",
                    "PASSWORD",
                    "CREDENTIAL",
                    "API_KEY",
                    "ACCESS_KEY",
                ]
                .iter()
                .any(|part| name.contains(part))
            {
                command.env_remove(key);
            }
        }
        let log_path = attempt.join(format!("commercial-{}.log", evidence.len()));
        let log = fs::File::create(&log_path)?;
        command
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("SUSI_HOME", home.join("susi"))
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_TERM_COLOR", "never")
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        let share = commercial_command_share(current_gate_share(), &args, cfg!(windows));
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let mut status = command.status()?;
        if !status.success() && gate_out_of_space(&fs::read(&log_path).unwrap_or_default()) {
            let gate = args.first().map_or("?", String::as_str);
            if let Some(line) = reclaim_for_rerun(candidate, &log_path, &mut command, gate)? {
                evidence.push(line);
                status = command.status()?;
            }
        }
        let code = status.code().unwrap_or(1);
        let line = format!(
            "cargo {} => exit code: {code}; elapsed-ms={}",
            args.join(" "),
            started.elapsed().as_millis()
        );
        record_phase(
            &format!("gate cargo {}", args.first().map_or("?", String::as_str)),
            started.elapsed().as_millis(),
        );
        evidence.push(line.clone());
        if !status.success() {
            let prefix = out_of_space_prefix(candidate, &fs::read(&log_path).unwrap_or_default());
            return Err(format!("gate failed: {prefix}{line}; log {}", log_path.display()).into());
        }
    }
    require_ship_evidence(&evidence)?;
    Ok(evidence)
}

/// The agent's own fast check: format, then clippy and tests for only the
/// packages its changes (committed, staged, unstaged and untracked) can affect
/// since it diverged from `origin/main`. It builds in the agent's own worktree
/// target with a private home, so parallel agents share no build directory,
/// runtime state or credentials. Advice before `commit`; the serialized
/// `integrate`/`ship` gate stays the acceptance.
fn check(root: &Path) -> Result<()> {
    let top = PathBuf::from(git(root, &["rev-parse", "--show-toplevel"])?);
    move_running_tool_out_of_the_target(&top);
    let base = git(&top, &["merge-base", "HEAD", "origin/main"])?;
    let changed = changed_paths(&top, &base)?;
    let mut gates: Vec<Vec<String>> = vec![
        vec!["fmt".into(), "--all".into(), "--check".into()],
        [
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ]
        .iter()
        .map(|arg| (*arg).into())
        .collect(),
        ["nextest", "run", "--workspace", "--locked"]
            .iter()
            .map(|arg| (*arg).into())
            .collect(),
    ];
    let nextest = nextest_requested();
    select_test_runner(&mut gates[2], nextest);
    let workspace = affected::load(&top)?;
    let selection = affected::select(&workspace, &changed);
    let workspace_selected = matches!(selection, affected::Selection::Workspace(_));
    let complete_workspace = !nextest && workspace_selected && complete_workspace_requested();
    if complete_workspace {
        test_binary_evidence::require_supported_source_inputs()?;
    }
    match selection {
        affected::Selection::Packages(packages) => {
            println!(
                "check: {} changed paths since {}; {} packages: {}",
                changed.len(),
                &base[..base.len().min(12)],
                packages.len(),
                packages.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            for args in &mut gates {
                affected::narrow(args, &packages);
            }
            include_release_tooling_for_xtask(&mut gates, &packages);
        }
        affected::Selection::Workspace(reason) => {
            println!(
                "check: {} changed paths; whole workspace ({reason})",
                changed.len()
            );
            vendor_regressions::include_release_tooling(&top, &mut gates);
        }
    }
    if workspace_selected && top.join("crates/susi-core/Cargo.toml").is_file() {
        let all: BTreeSet<String> = ["xtask".to_owned()].into();
        include_release_tooling_for_xtask(&mut gates, &all);
    }
    // A narrowed `cargo test` gate runs through the concurrent binary runner
    // (see `test_runner`), which leaves doctests to the companion gate.
    let runner_tests =
        !nextest && !test_runner::serial_requested() && (!workspace_selected || complete_workspace);
    if complete_workspace {
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[2].iter().cloned())
            .collect();
        gates.push(test_binary_evidence::doc_argv(&logical)?[1..].to_vec());
    } else if nextest {
        if let Some(doc) = doc_gate(&gates[2], Some(&workspace.libraries)) {
            gates.push(doc);
        }
    } else if runner_tests {
        let mut as_runner: Vec<String> = vec!["test".into(), "run".into()];
        as_runner.extend(gates[2].iter().skip(1).cloned());
        if let Some(doc) = doc_gate(&as_runner, Some(&workspace.libraries)) {
            gates.push(doc);
        }
    }
    let original_home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("cannot identify toolchain home")?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let target = top.join("target");
    if complete_workspace {
        let (_, host) = gate_receipt::rustc_version(&top)?;
        for args in gates
            .iter_mut()
            .filter(|args| args.first().is_some_and(|a| a == "test"))
        {
            args.extend([
                "--target".into(),
                host.clone(),
                "--target-dir".into(),
                text(&target)?.into(),
            ]);
        }
        let logical: Vec<String> = std::iter::once("cargo".into())
            .chain(gates[2].iter().cloned())
            .collect();
        gates[2] = test_binary_evidence::build_argv(&logical)?[1..].to_vec();
    }
    seed_worktree_target(&top, &top, false);
    // The same refusal a ship makes: a check that would run out of disk
    // fails after its compile time with an error that reads like a compiler
    // bug, so a doomed one is refused first, after the idle caches are
    // reclaimed.
    timed("gate disk check", || {
        require_gate_disk(&top, &target, workspace_selected)
    })?;
    let home = target.join("susi-check-home");
    fs::create_dir_all(home.join("AppData/Local"))?;
    // Only this worktree's own target is locked: checks in other worktrees
    // and ships run at the same time, each command on its share of the host.
    let _host = acquire_host_gate(
        &target.join("check-gate"),
        &format!("pid={} check={}", std::process::id(), top.display()),
    )?;
    let _live = live_gate();
    let total = std::time::Instant::now();
    let mut pending_binary = None;
    for args in &gates {
        let started = std::time::Instant::now();
        let mut command = check_command(&top, args, &target, &home, (&cargo_home, &rustup_home));
        let share = current_gate_share();
        apply_gate_share(&mut command, share);
        print_gate_share(args.first().map_or("?", String::as_str), share);
        let binaries_gate = runner_tests
            && args.first().map(String::as_str) == Some("test")
            && !args.iter().any(|arg| arg == "--doc");
        let status = if binaries_gate && complete_workspace {
            let log = home.join("complete-workspace.log");
            fs::write(&log, [])?;
            pending_binary = Some(run_complete_binaries(&TestBinariesGate {
                candidate: &top,
                attempt: &home,
                args,
                host: "",
                target: &target,
                log_path: &log,
                share,
                env: GateEnv {
                    home: &home,
                    cargo_home: &cargo_home,
                    rustup_home: &rustup_home,
                    target: &target,
                    sysroot: None,
                },
            })?);
            exit_status(0)
        } else if binaries_gate {
            check_test_binaries(
                &CheckGate {
                    top: &top,
                    target: &target,
                    home: &home,
                    homes: (&cargo_home, &rustup_home),
                    share,
                },
                args,
            )?
        } else {
            command.status()?
        };
        println!(
            "check: cargo {} => {} in {:.1}s",
            args.first().map_or("", String::as_str),
            if status.success() { "ok" } else { "FAILED" },
            started.elapsed().as_secs_f64()
        );
        if !status.success() {
            return Err(format!("check failed: cargo {}", args.join(" ")).into());
        }
    }
    if let Some((execution, mut descriptor)) = pending_binary {
        native_provenance::finish_binary_plan(&top, &mut descriptor, &execution)?;
        execution.recheck()?;
        record_phase(
            "check complete binaries including final input checks",
            execution.elapsed_ms(),
        );
    }
    println!("check: passed in {:.1}s", total.elapsed().as_secs_f64());
    Ok(())
}

/// One `check` gate command: the worktree's own target, an isolated home for
/// the tests (HOME, USERPROFILE, APPDATA, LOCALAPPDATA, XDG_*, SUSI_HOME), and
/// the operator's real CARGO_HOME and RUSTUP_HOME, resolved before the home
/// override. Without the last two, tests that run cargo themselves (such as
/// `tests/zc_matrix.rs`'s `cargo tree --offline`) resolve an empty private
/// cargo home and fail offline, as they did under `check` before 2026-11-06.
fn check_command(
    top: &Path,
    args: &[String],
    target: &Path,
    home: &Path,
    homes: (&Path, &Path),
) -> Command {
    let mut command = Command::new("cargo");
    check_env(&mut command, target, home, homes);
    command.current_dir(top).args(args);
    command
}

/// `cargo local-workflow check` runs this executable from
/// `<worktree>/target/debug/`, and a check that selects `xtask` rebuilds
/// that very file: the alias builds it without `release-tooling`, the gate
/// with it, so each relinks the same path. Windows refuses to replace a
/// running program ("Access is denied"), which failed every xtask check
/// until 2026-11-07. A running program may be renamed, though, so the file
/// moves aside (to `<target>/check-exe/`) before the gates run: Cargo then
/// links a fresh `local-workflow.exe` beside it, and the next alias build
/// links one again. Best effort: when the move fails the gate runs as before.
fn move_running_tool_out_of_the_target(top: &Path) {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let target = top.join("target");
    if !exe.starts_with(&target) {
        return;
    }
    let aside = target.join("check-exe");
    if fs::create_dir_all(&aside).is_err() {
        return;
    }
    // Files from earlier checks: whichever are no longer running go now.
    if let Ok(entries) = fs::read_dir(&aside) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }
    let moved = aside.join(format!(
        "local-workflow-{}{}",
        std::process::id(),
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let _ = fs::rename(&exe, &moved);
}

/// The `check` environment for a cargo command or one of the test binaries
/// it built: see `check_command`.
fn check_env(
    command: &mut Command,
    target: &Path,
    home: &Path,
    (cargo_home, rustup_home): (&Path, &Path),
) {
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "CREDENTIAL",
                "API_KEY",
                "ACCESS_KEY",
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    command
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("SUSI_HOME", home.join("susi"))
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"));
}

/// `check`'s narrowed test gate: Cargo builds the binaries, the concurrent
/// runner executes them (each in its own home under the worktree target),
/// and every binary's output goes to `<target>/check-tests.log`.
#[derive(Clone, Copy)]
struct CheckGate<'a> {
    top: &'a Path,
    target: &'a Path,
    home: &'a Path,
    homes: (&'a Path, &'a Path),
    share: GateShare,
}

fn check_test_binaries(gate: &CheckGate<'_>, args: &[String]) -> Result<std::process::ExitStatus> {
    let CheckGate {
        top,
        target,
        home,
        homes,
        share,
    } = *gate;
    let log_path = target.join("check-tests.log");
    fs::write(&log_path, b"")?;
    let mut build = check_command(top, args, target, home, homes);
    build
        .args(["--no-run", "--message-format=json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    apply_gate_share(&mut build, share);
    let started = std::time::Instant::now();
    let output = build.output()?;
    let (binaries, diagnostics) =
        test_runner::parse_artifacts(&String::from_utf8_lossy(&output.stdout))?;
    for rendered in &diagnostics {
        eprint!("{rendered}");
    }
    println!(
        "check: test build => {} in {:.1}s; {} test binaries",
        if output.status.success() {
            "ok"
        } else {
            "FAILED"
        },
        started.elapsed().as_secs_f64(),
        binaries.len()
    );
    if !output.status.success() {
        return Ok(output.status);
    }
    let lanes = test_runner::concurrency(share.jobs);
    let heavy = test_runner::heavy_binaries(top);
    let home_root = target.join("susi-check-homes");
    let durations = target.join("test-durations.json");
    let settings = test_runner::Settings {
        concurrency: lanes,
        total_test_threads: share.test_threads,
        build_jobs: test_runner::threads_per_binary(share.jobs, lanes),
        home_root: &home_root,
        target_dir: target,
        triple: None,
        durations: &durations,
        heavy: &heavy,
        log: &log_path,
    };
    println!(
        "check: {} test binaries on up to {} lanes, {} aggregate test-thread budget; output in {}",
        binaries.len(),
        lanes,
        settings.total_test_threads,
        log_path.display()
    );
    let launch = move |binary: &test_runner::Binary, bin_home: &Path| -> Result<Command> {
        let mut command = Command::new(&binary.executable);
        check_env(&mut command, target, bin_home, homes);
        Ok(command)
    };
    let outcome = test_runner::run(&binaries, &settings, &launch)?;
    let _ = fs::remove_dir_all(&home_root);
    for (id, code) in &outcome.failed {
        println!(
            "check: {id} FAILED ({})",
            code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}")
            )
        );
    }
    let complete = outcome.failed.is_empty() && outcome.ran == binaries.len();
    println!(
        "check: {} of {} test binaries ran in {:.1}s, {} failed",
        outcome.ran,
        binaries.len(),
        outcome.elapsed_ms as f64 / 1000.0,
        outcome.failed.len()
    );
    Ok(exit_status(i32::from(!complete)))
}

/// A verification slot that a ship currently holds.
#[derive(Debug, Eq, PartialEq)]
struct BusySlot {
    slot: usize,
    owner: String,
}

/// A point-in-time view of the local workflow. It never takes the workflow
/// lock or waits on a slot, so it answers while other agents start, commit,
/// integrate or ship.
#[derive(Debug)]
struct WorkflowStatus {
    integration_head: Option<String>,
    claims: Vec<Claim>,
    busy_slots: Vec<BusySlot>,
}

fn workflow_status(root: &Path) -> Result<WorkflowStatus> {
    let state = state(root)?;
    Ok(WorkflowStatus {
        integration_head: integration_head(root)?,
        claims: claims(&state)?,
        busy_slots: busy_slots(&state.join("slots"))?,
    })
}

/// Probe each `<n>.lock` with a non-blocking try-lock: a held lock is a busy
/// slot; a free one is released at once when the probe handle drops.
fn busy_slots(slots: &Path) -> Result<Vec<BusySlot>> {
    let entries = match fs::read_dir(slots) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut busy = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(slot) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".lock"))
            .and_then(|index| index.parse::<usize>().ok())
        else {
            continue;
        };
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        match file.try_lock() {
            Ok(()) => release_probe(file),
            Err(fs::TryLockError::WouldBlock) => busy.push(BusySlot {
                slot,
                owner: fs::read_to_string(slots.join(slot.to_string()).join("owner"))
                    .map(|owner| owner.trim().to_owned())
                    .unwrap_or_default(),
            }),
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    busy.sort_by_key(|busy| busy.slot);
    Ok(busy)
}

fn status_json(status: &WorkflowStatus) -> serde_json::Value {
    serde_json::json!({
        "schema": "susi-local-workflow-status-v1",
        "integration_head": status.integration_head,
        "claims": status.claims.iter().map(|claim| serde_json::json!({
            "name": claim.name,
            "worktree": claim.worktree.display().to_string(),
            "local": claim.is_local(),
            "base": claim.base,
            "scopes": claim.scopes,
        })).collect::<Vec<_>>(),
        "busy_slots": status.busy_slots.iter().map(|busy| serde_json::json!({
            "slot": busy.slot,
            "owner": busy.owner,
        })).collect::<Vec<_>>(),
    })
}

fn status_text(status: &WorkflowStatus) -> String {
    let mut out = format!(
        "integration: {}\nclaims: {}\n",
        status.integration_head.as_deref().unwrap_or("none"),
        status.claims.len()
    );
    for claim in &status.claims {
        out.push_str(&format!(
            "  {}\t{}{}\tbase {}\t{}\n",
            claim.name,
            claim.worktree.display(),
            claim.host_label(),
            claim.base.get(..12).unwrap_or(&claim.base),
            claim.scopes.join(", ")
        ));
    }
    out.push_str(&format!("verifying: {}\n", status.busy_slots.len()));
    for busy in &status.busy_slots {
        let owner = if busy.owner.is_empty() {
            "unknown owner"
        } else {
            &busy.owner
        };
        out.push_str(&format!("  slot {}\t{owner}\n", busy.slot));
    }
    out
}

fn run(args: &[String]) -> Result<()> {
    let root = env::current_dir()?;
    match args.first().map(String::as_str) {
        Some("provenance-policy") if args.len() == 1 => {
            println!("{}", serde_json::json!({"schema":"susi-native-provenance-policy-v1",
                "policy_sha256":native_provenance::policy_digest(),"policy":native_provenance::POLICY}));
        },
        Some("verify-provenance") => {
            let mut flags = std::collections::BTreeMap::new();
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                if !["--root","--permit","--permit-sha256","--verifier-sha256","--policy-sha256","--head"].contains(&flag.as_str())
                    || flags.insert(flag.as_str(), rest.next().ok_or("missing provenance flag value")?.as_str()).is_some() {
                    return Err("unknown or duplicate provenance argument".into());
                }
            }
            let get = |key| flags.get(key).copied().ok_or("missing required provenance argument");
            let report = native_provenance::verify(Path::new(get("--root")?), Path::new(get("--permit")?),
                &native_provenance::TrustPins {permit_sha256:get("--permit-sha256")?,
                    verifier_sha256:get("--verifier-sha256")?,policy_sha256:get("--policy-sha256")?}, get("--head")?)?;
            println!("{}",serde_json::to_string(&report)?);
        },
        Some("start" | "extend" | "shrink") => {
            let claimant = args.get(1).ok_or("start requires an agent name")?;
            let mut scopes = Vec::new();
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                if flag != "--scope" { return Err(format!("unknown argument: {flag}").into()); }
                scopes.push(rest.next().ok_or("--scope requires a path")?.clone());
            }
            if args[0] == "extend" { update_scopes(&root, claimant, &scopes, ScopeChange::Extend)?; return Ok(()); }
            if args[0] == "shrink" { update_scopes(&root, claimant, &scopes, ScopeChange::Shrink)?; return Ok(()); }
            let worktree = start(&root, claimant, &scopes)?;
            println!("Worktree: {}\nTarget: {}\nUse the local-build entrypoint to override inherited target directories. Reservations cover cooperating launcher users only.", worktree.display(), worktree.join("target").display());
        }
        Some("release") if args.len() == 2 => release(&root, &args[1])?,
        Some("sync" | "step") if (2..=3).contains(&args.len()) => {
            let json_out = if args.len() == 3 {
                if args[2] != "--json" {
                    return Err("unknown sync flag; expected --json".into());
                }
                true
            } else {
                false
            };
            let report = sync_claim(&root, &args[1])?;
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": "susi-local-workflow-sync-v1",
                        "claimant": report.claimant,
                        "mode": report.mode.as_str(),
                        "previous_head": report.previous_head,
                        "head": report.head,
                        "upstream": report.upstream,
                        "claim_base": report.claim_base,
                    }))?
                );
            } else {
                println!(
                    "Synced {} ({}): {} -> {} (base {})",
                    report.claimant,
                    report.mode.as_str(),
                    report.previous_head,
                    report.head,
                    report.claim_base
                );
            }
        }
        Some("commit") if args.len() == 3 => commit(&root, &args[1], &args[2], None)?,
        Some("commit") if args.len() == 4 && args[3] == "--no-pull" => {
            commit_with_pull(&root, &args[1], &args[2], None, false)?;
        }
        Some("finish-merge") if args.len() == 4 => scoped_commit(&root, &args[1], &args[3], None, Some(&args[2]))?,
        Some("finish-merge") if args.iter().any(|arg| arg == "--sign") => return Err("finish-merge does not support signing".into()),
        Some("commit") if args.len() == 5 && args[3] == "--sign" => commit(&root, &args[1], &args[2], Some(&args[4]))?,
        Some("distill") if args.len() >= 2 => {
            println!("{}", workflow_distill(&root, &args[1..])?);
        }
        Some("integrate") if args.len() >= 2 => {
            let started = std::time::Instant::now();
            let names: Vec<_> = args[1..].iter().map(String::as_str).collect();
            let result = integrate_with(&root, &names, verify_candidate);
            finish_phases(&root, "integrate", if result.is_ok() { "integrated" } else { "failed" }, started.elapsed());
            result?;
        }
        Some("list") if args.len() == 1 => {
            let state = state(&root)?;
            let _lock = Lock::acquire(&state)?;
            for claim in claims(&state)? { println!("{}\t{}{}\t{}", claim.name, claim.worktree.display(), claim.host_label(), claim.scopes.join(", ")); }
        }
        Some("status") if args.len() == 1 => print!("{}", status_text(&workflow_status(&root)?)),
        Some("status") if args.len() == 2 && args[1] == "--json" => {
            println!("{}", serde_json::to_string_pretty(&status_json(&workflow_status(&root)?))?);
        }
        Some("check") if args.len() == 1 => check(&root)?,
        Some("affected") if (2..=3).contains(&args.len()) => {
            let head = args.get(2).map_or("HEAD", String::as_str);
            let changed = affected::changed(&root, &args[1], head)?;
            let (selection, why) = affected::select_explained(&affected::load(&root)?, &changed);
            match selection {
                affected::Selection::Packages(packages) => {
                    println!("{} changed paths; gate covers {} packages:", changed.len(), packages.len());
                    for package in packages {
                        let reason = why.get(&package).map_or("", String::as_str);
                        println!("  {package}  ({reason})");
                    }
                }
                affected::Selection::Workspace(reason) => {
                    println!("{} changed paths; gate covers the whole workspace: {reason}", changed.len());
                }
            }
        }
        Some("ship") if args.get(1).map(String::as_str) == Some("--retry") && args.len() == 2 => {
            let result = ship_retry(&root, &mut GitRemote);
            witness_storage(&root);
            print_ship(&result?);
        }
        Some("ship") if args.len() >= 2 => {
            let started = std::time::Instant::now();
            let names: Vec<_> = args[1..].iter().map(String::as_str).collect();
            let mut remote = GitRemote;
            let result = repo_kind(&root).and_then(|kind| match kind {
                RepoKind::Susi => ship(&root, &names, &mut remote, verify_candidate),
                RepoKind::Commercial => ship(&root, &names, &mut remote, commercial_gate),
            });
            finish_phases(&root, "ship", if result.is_ok() { "shipped" } else { "failed" }, started.elapsed());
            witness_storage(&root);
            print_ship(&result?);
        }
        _ => return Err("usage: local-workflow start <name> --scope <path> [--scope <path> ...] | extend <name> --scope <path> | shrink <name> --scope <path> | sync <name> [--json] | step <name> [--json] | list | status [--json] | commit <name> <message> [--sign <fingerprint> | --no-pull] | finish-merge <name> <parent-oid> <message> | distill <args...> | integrate <name> [<name> ...] | release <name> | ship <name> [<name> ...] | ship --retry | check | affected <base> [<head>]".into()),
    }
    Ok(())
}

/// After a ship, whatever its result, this clone witnesses the storage remote
/// (`storage_guard`), in Susi core and in Susi Commercial Apps alike: lost
/// history is reported on stderr and never changes the ship's result.
fn witness_storage(root: &Path) {
    if let Some(advisory) = storage_guard::witness(root, "origin") {
        eprintln!("local-workflow: {advisory}");
    }
}

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("local-workflow") {
        args.remove(0);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("local-workflow: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[test]
    fn workflow_context_rejects_a_foreign_lock_directory_before_loading() {
        let (_scratch, repo, base) = ship_fixture("one");
        let foreign = Scratch::new();
        let other = foreign.repo();
        assert!(load_sources(&repo, &state(&other).unwrap(), &["one"], &base).is_err());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn workflow_context_rechecks_root_and_source_gitfile_after_the_gate() {
        for redirect_source in [false, true] {
            let (scratch, repo, base) = ship_fixture("one");
            let foreign = Scratch::new();
            let other = foreign.repo();
            let entry = scratch.0.join("entry");
            git(
                &repo,
                &[
                    "-c",
                    "core.hooksPath=",
                    "worktree",
                    "add",
                    "--detach",
                    text(&entry).unwrap(),
                    &base,
                ],
            )
            .unwrap();
            let claim = read_claim(&state(&repo).unwrap().join("claims/one.claim")).unwrap();
            let redirected = if redirect_source {
                claim.worktree.join(".git")
            } else {
                entry.join(".git")
            };
            let original = fs::read(&redirected).unwrap();
            let mut remote = FakeRemote::single(base);
            let result = ship(&entry, &["one"], &mut remote, |_candidate, _attempt| {
                fs::write(
                    &redirected,
                    format!("gitdir: {}\n", other.join(".git").display()),
                )
                .unwrap();
                Ok(local_gate_evidence())
            });
            fs::write(&redirected, original).unwrap();
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains(if redirect_source {
                    "claim repository"
                } else {
                    "workflow directory changed"
                }),
                "{error}"
            );
            assert!(remote.pushes.is_empty());
            assert!(integration_head(&repo).unwrap().is_none());
            assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
        }
    }

    #[test]
    fn timing_record_preserves_nested_phases_without_calling_the_sum_elapsed() {
        let phases = vec![
            ("build".to_owned(), 100),
            ("binaries".to_owned(), 200),
            ("workspace".to_owned(), 300),
        ];
        let record = timing_record(
            7,
            "ship",
            "shipped",
            std::time::Duration::from_millis(450),
            &phases,
        );
        assert_eq!(record["schema"], "susi-local-workflow-timings-v2");
        assert_eq!(record["elapsed_ms"], 450);
        assert_eq!(record["inclusive_phase_sum_ms"], 600);
        assert_eq!(record["phases_ms"].as_array().unwrap().len(), 3);
        assert!(record.get("total_ms").is_none());
    }

    #[test]
    fn timing_record_includes_unphased_time_and_keeps_failure_outcome() {
        let record = timing_record(
            8,
            "integrate",
            "failed",
            std::time::Duration::from_millis(125),
            &[],
        );
        assert_eq!(record["elapsed_ms"], 125);
        assert_eq!(record["inclusive_phase_sum_ms"], 0);
        assert_eq!(record["outcome"], "failed");
        assert_eq!(record["command"], "integrate");
        assert!(record["elapsed_scope"]
            .as_str()
            .unwrap()
            .contains("retries"));
    }

    #[test]
    fn batched_git_paths_match_individual_queries_and_preserve_every_guard() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "markers", &["owned.rs".into()]).unwrap();
        let names = [
            "MERGE_HEAD",
            "MERGE_AUTOSTASH",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "sequencer",
            "rebase-merge",
            "rebase-apply",
        ];
        for root in [&repo, &worktree] {
            let paths = git_paths(root, &names).unwrap();
            for (name, path) in names.iter().zip(paths) {
                let individual = git(
                    root,
                    &["rev-parse", "--path-format=absolute", "--git-path", name],
                )
                .unwrap();
                assert_eq!(path, PathBuf::from(individual));
            }
        }
        fs::write(worktree.join("owned.rs"), "change").unwrap();
        let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        for path in git_paths(&worktree, &names).unwrap() {
            fs::write(&path, "operation in progress").unwrap();
            assert!(commit(&worktree, "markers", "must refuse", None).is_err());
            assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
            fs::remove_file(path).unwrap();
        }
        commit(&worktree, "markers", "all guards absent", None).unwrap();
        assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
    }

    #[test]
    fn batched_git_paths_reject_incomplete_relative_and_extra_output() {
        let root = env::temp_dir();
        let absolute = root.display().to_string();
        assert!(parse_git_paths(&absolute, 1).is_ok());
        assert!(parse_git_paths("", 1).is_err());
        assert!(parse_git_paths("relative/path", 1).is_err());
        assert!(parse_git_paths(&absolute, 2).is_err());
        assert!(parse_git_paths(&format!("{absolute}\n{absolute}"), 1).is_err());
    }

    #[test]
    fn workspace_proof_waits_for_every_planned_gate() {
        assert!(completed_workspace_proof("key", Some("key"), 0, 0).is_none());
        for completed in 0..6 {
            assert!(completed_workspace_proof("key", Some("key"), completed, 6).is_none());
        }
        assert!(completed_workspace_proof("key", Some("key"), 7, 6).is_none());
        assert_eq!(
            completed_workspace_proof("key", Some("key"), 6, 6),
            Some(workspace_test_proof::record("key"))
        );
    }

    #[test]
    fn workspace_proof_rechecks_inputs_after_companion_gates() {
        assert!(completed_workspace_proof("before", Some("after"), 6, 6).is_none());
        assert!(completed_workspace_proof("before", None, 6, 6).is_none());
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "susi-local-workflow-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn repo(&self) -> PathBuf {
            let repo = self.0.join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]).unwrap();
            git(&repo, &["config", "user.name", "Test"]).unwrap();
            git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&repo, &["config", "commit.gpgsign", "false"]).unwrap();
            fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
            git(&repo, &["add", ".gitignore"]).unwrap();
            git(
                &repo,
                &[
                    "-c",
                    "core.hooksPath=",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-qm",
                    "initial",
                ],
            )
            .unwrap();
            repo
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            if std::thread::panicking() {
                return;
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tracked_file_fixture(
        claimant: &str,
        path: &str,
        scope_path: &str,
        contents: &str,
    ) -> (Scratch, PathBuf, PathBuf) {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let file = repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, contents).unwrap();
        git(&repo, &["add", "--", path]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "fixture file"],
        )
        .unwrap();
        let worktree = start(&repo, claimant, &[scope_path.to_owned()]).unwrap();
        (scratch, repo, worktree)
    }

    #[test]
    fn commit_only_stages_an_already_staged_deletion_without_readding_its_path() {
        let (_scratch, repo, worktree) =
            tracked_file_fixture("staged-delete", "src/gone.txt", "src", "base\n");
        fs::remove_file(worktree.join("src/gone.txt")).unwrap();
        git(&worktree, &["add", "-u", "--", "src/gone.txt"]).unwrap();
        let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        commit(&worktree, "staged-delete", "delete tracked file", None).unwrap();
        assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
        assert!(git(&worktree, &["cat-file", "-e", "HEAD:src/gone.txt"]).is_err());
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    fn commit_only_stages_an_unstaged_deletion() {
        let path = "src/a [file].txt";
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("unstaged-delete", path, "src", "base\n");
        fs::remove_file(worktree.join(path)).unwrap();
        commit(&worktree, "unstaged-delete", "delete tracked file", None).unwrap();
        assert!(git(&worktree, &["cat-file", "-e", "HEAD:src/a [file].txt"]).is_err());
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn commit_only_stages_a_previously_staged_rename() {
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("staged-rename", "src/old.txt", "src", "base\n");
        git(&worktree, &["mv", "--", "src/old.txt", "src/new.txt"]).unwrap();
        commit(&worktree, "staged-rename", "rename tracked file", None).unwrap();
        let names = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        assert!(names.lines().any(|name| name == "src/new.txt"), "{names}");
        assert!(!names.lines().any(|name| name == "src/old.txt"), "{names}");
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn commit_only_stages_the_unstaged_side_of_a_staged_revert() {
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("staged-revert", "src/value.txt", "src", "base\n");
        fs::write(worktree.join("src/value.txt"), "staged\n").unwrap();
        git(&worktree, &["add", "--", "src/value.txt"]).unwrap();
        fs::write(worktree.join("src/value.txt"), "base\n").unwrap();
        let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        assert!(commit(&worktree, "staged-revert", "revert staged content", None).is_err());
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn commit_only_stages_final_content_after_a_second_unstaged_edit() {
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("staged-final", "src/value.txt", "src", "base\n");
        fs::write(worktree.join("src/value.txt"), "staged intermediate\n").unwrap();
        git(&worktree, &["add", "--", "src/value.txt"]).unwrap();
        fs::write(worktree.join("src/value.txt"), "final content\n").unwrap();
        commit(&worktree, "staged-final", "commit final content", None).unwrap();
        assert_eq!(
            git(&worktree, &["show", "HEAD:src/value.txt"]).unwrap(),
            "final content"
        );
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn commit_only_stages_a_scoped_windows_case_only_rename() {
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("case-rename", "src/Task.json", "src", "base\n");
        git(
            &worktree,
            &["mv", "--force", "--", "src/Task.json", "src/task.json"],
        )
        .unwrap();
        commit(&worktree, "case-rename", "rename task casing", None).unwrap();
        let names = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        assert!(names.lines().any(|name| name == "src/task.json"), "{names}");
        assert!(
            !names.lines().any(|name| name == "src/Task.json"),
            "{names}"
        );
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn commit_only_stages_exact_removal_from_a_duplicate_case_index() {
        let (_scratch, _repo, worktree) =
            tracked_file_fixture("case-index", "src/Task.json", "src", "base\n");
        let blob = git(&worktree, &["rev-parse", "HEAD:src/Task.json"]).unwrap();
        let duplicate = format!("100644,{blob},src/task.json");
        git(
            &worktree,
            &["update-index", "--add", "--cacheinfo", &duplicate],
        )
        .unwrap();
        git(
            &worktree,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-qm",
                "duplicate case fixture",
            ],
        )
        .unwrap();
        let index = git(&worktree, &["ls-files"]).unwrap();
        assert!(index.lines().any(|path| path == "src/Task.json"), "{index}");
        assert!(index.lines().any(|path| path == "src/task.json"), "{index}");

        git(
            &worktree,
            &["update-index", "--force-remove", "--", "src/task.json"],
        )
        .unwrap();
        let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        commit(
            &worktree,
            "case-index",
            "remove duplicate lowercase entry",
            None,
        )
        .unwrap();
        assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
        let tree = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
        assert!(tree.lines().any(|path| path == "src/Task.json"), "{tree}");
        assert!(!tree.lines().any(|path| path == "src/task.json"), "{tree}");
        assert!(worktree.join("src/Task.json").is_file());
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn commit_only_stages_rejects_staged_out_of_scope_deletion() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        fs::write(repo.join("outside.txt"), "base\n").unwrap();
        git(&repo, &["add", "--", "outside.txt"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside fixture"],
        )
        .unwrap();
        let worktree = start(&repo, "outside-delete", &["src".into()]).unwrap();
        fs::remove_file(worktree.join("outside.txt")).unwrap();
        git(&worktree, &["add", "-u", "--", "outside.txt"]).unwrap();
        let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        let error = commit(&worktree, "outside-delete", "reject outside delete", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved scopes"), "{error}");
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
        assert_eq!(
            git(&worktree, &["diff", "--cached", "--name-status"]).unwrap(),
            "D\toutside.txt"
        );
    }

    #[test]
    fn pinned_merge_requires_a_resolved_scoped_index_and_preserves_both_parents() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        let worker = start(&repo, "merge", &["docs".into()]).unwrap();
        fs::create_dir(worker.join("docs")).unwrap();
        fs::write(worker.join("docs/record"), "worker").unwrap();
        commit(&worker, "merge", "worker change", None).unwrap();
        let own = git(&worker, &["rev-parse", "HEAD"]).unwrap();
        fs::create_dir(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/record"), "incoming").unwrap();
        git(&repo, &["add", "docs/record"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "incoming change"],
        )
        .unwrap();
        let incoming = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let refs = || {
            git(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            )
            .unwrap()
        };
        let before = refs();
        assert!(git(&worker, &["merge", "--no-ff", "--no-commit", &incoming]).is_err());
        let finish = |parent: &str| scoped_commit(&worker, "merge", "join", None, Some(parent));
        assert!(finish(&incoming).is_err()); // Unresolved add/add conflict.
        fs::write(worker.join("docs/record"), "worker and incoming").unwrap();
        git(&worker, &["add", "docs/record"]).unwrap();
        assert!(commit(&worker, "merge", "normal commit", None).is_err());
        assert!(finish(&own).is_err());
        assert!(scoped_commit(
            &worker,
            "merge",
            "join",
            Some(&"a".repeat(40)),
            Some(&incoming)
        )
        .is_err());

        fs::write(worker.join("outside"), "untracked").unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["add", "outside"]).unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["reset", "--", "outside"]).unwrap();
        fs::remove_file(worker.join("outside")).unwrap();
        fs::write(worker.join("docs/record"), "unstaged").unwrap();
        assert!(finish(&incoming).is_err());
        git(&worker, &["restore", "--worktree", "--", "docs/record"]).unwrap();
        let git_path = |name: &str| {
            PathBuf::from(
                git(
                    &worker,
                    &["rev-parse", "--path-format=absolute", "--git-path", name],
                )
                .unwrap(),
            )
        };
        let merge_head = git_path("MERGE_HEAD");
        fs::write(&merge_head, format!("{incoming}\n{own}\n")).unwrap();
        assert!(finish(&incoming).is_err());
        fs::write(&merge_head, format!("{incoming}\n")).unwrap();
        let sequencer = git_path("sequencer");
        fs::create_dir(&sequencer).unwrap();
        assert!(finish(&incoming).is_err());
        fs::remove_dir(sequencer).unwrap();
        assert_eq!(before, refs());

        let tree = git(&worker, &["write-tree"]).unwrap();
        finish(&incoming).unwrap();
        let merged = git(&worker, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            git(&worker, &["show", "-s", "--format=%T %P", "HEAD"]).unwrap(),
            format!("{tree} {own} {incoming}")
        );
        assert_eq!(
            refs(),
            before.replace(
                &format!("refs/heads/local-agent/merge {own}"),
                &format!("refs/heads/local-agent/merge {merged}")
            )
        );
        assert!(git(&worker, &["status", "--porcelain"]).unwrap().is_empty());
        assert!(!merge_head.exists());

        // Simulate interruption after the compare-and-swap, before Git-state cleanup.
        fs::write(&merge_head, format!("{incoming}\n")).unwrap();
        finish(&incoming).unwrap();
        assert_eq!(git(&worker, &["rev-parse", "HEAD"]).unwrap(), merged);
        assert!(!merge_head.exists());
    }

    #[test]
    fn extension_preserves_base_and_rejects_conflicts_without_mutation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let first = start(&repo, "first", &["src/first".into()]).unwrap();
        start(&repo, "second", &["src/second".into()]).unwrap();
        let claim_path = state(&repo).unwrap().join("claims/first.claim");
        let base = read_claim(&claim_path).unwrap().base;
        update_scopes(&first, "first", &["tests".into()], ScopeChange::Extend).unwrap();
        let claim = read_claim(&claim_path).unwrap();
        assert_eq!(claim.base, base);
        assert!(claim.scopes.contains(&"tests".into()));
        let before = fs::read(&claim_path).unwrap();
        assert!(update_scopes(&first, "first", &["src".into()], ScopeChange::Extend).is_err());
        assert!(update_scopes(&repo, "first", &["docs".into()], ScopeChange::Extend).is_err());
        fs::write(first.join("dirty"), "pending").unwrap();
        assert!(update_scopes(&first, "first", &["docs".into()], ScopeChange::Extend).is_err());
        assert_eq!(fs::read(claim_path).unwrap(), before);
    }

    #[test]
    fn a_claim_recorded_on_another_os_is_listed_and_still_reserves_its_scopes() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let state = state(&repo).unwrap();
        let claims_dir = state.join("claims");
        fs::create_dir_all(&claims_dir).unwrap();
        let foreign = if cfg!(windows) {
            "/mnt/shared/src/repo-agent-far".to_owned()
        } else {
            "F:/src/github.com/intellibitz\\repo-agent-far".to_owned()
        };
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let recorded = format!("susi-local-claim-v2\nfar\n{foreign}\n{base}\nsrc/far\n");
        fs::write(claims_dir.join("far.claim"), &recorded).unwrap();

        let listed = claims(&state).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].is_local());
        assert_eq!(listed[0].worktree, PathBuf::from(&foreign));
        let status = workflow_status(&repo).unwrap();
        assert!(status_text(&status).contains("[other host]"));
        assert_eq!(status_json(&status)["claims"][0]["local"], false);

        let error = start(&repo, "near", &["src/far/x.rs".into()]).unwrap_err();
        assert!(error.to_string().contains("overlaps agent far"), "{error}");
        let worktree = start(&repo, "near", &["src/near".into()]).unwrap();
        assert!(claims(&state)
            .unwrap()
            .iter()
            .any(|c| c.name == "near" && c.is_local()));
        // Started from an agent worktree, the new one still sits beside the primary.
        let nested = start(&worktree, "nested", &["src/nested".into()]).unwrap();
        let primary_name = repo.file_name().unwrap().to_str().unwrap();
        // Compared as files, not strings: on a Windows host whose TEMP is an
        // 8.3 short path (C:\Users\ABCDEF~1\...), the scratch repo is named
        // that way while git reports the long form for the same directory.
        assert_eq!(
            fs::canonicalize(&nested).unwrap(),
            fs::canonicalize(
                repo.parent()
                    .unwrap()
                    .join(format!("{primary_name}-agent-nested"))
            )
            .unwrap()
        );

        let error = release(&repo, "far").unwrap_err();
        assert!(error.to_string().contains("another host"), "{error}");
        release(&worktree, "near").unwrap();
        release(&nested, "nested").unwrap();
        assert_eq!(
            fs::read_to_string(claims_dir.join("far.claim")).unwrap(),
            recorded
        );

        for relative in ["relative/path", "x:", "C:relative"] {
            fs::write(
                claims_dir.join("far.claim"),
                format!("susi-local-claim-v2\nfar\n{relative}\n{base}\nsrc/far\n"),
            )
            .unwrap();
            assert!(claims(&state).is_err(), "{relative}");
        }
    }

    #[test]
    fn path_validation_blocks_windows_aliases_and_traversal() {
        for bad in [
            "",
            ".",
            "../src",
            "a/../b",
            "C:/src",
            "\\\\host\\share",
            "src/",
            "src//x",
            "NUL.rs",
            "src/COM1",
            ".git/config",
            "src/a.",
            "src/a ",
            "src/a:b",
            "src/\nx",
        ] {
            assert!(scope(bad).is_err(), "{bad}");
        }
        assert_eq!(scope("Src\\CLI").unwrap(), "src/cli");
        assert!(overlaps("src", "src/cli"));
        assert!(!overlaps("src", "src2"));
        for bad in ["con", "aux", "a/b", "-abc", "abc-", "A", ""] {
            assert!(name(bad).is_err());
        }
    }

    #[test]
    fn unused_scope_can_transfer_without_orphaning_committed_changes() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(
            &repo,
            "builder",
            &["runtime.rs".into(), "Cargo.toml".into()],
        )
        .unwrap();
        fs::write(worktree.join("runtime.rs"), "changed").unwrap();
        commit(&worktree, "builder", "retain changed runtime", None).unwrap();
        let path = state(&repo).unwrap().join("claims/builder.claim");
        let original = fs::read(&path).unwrap();
        for scopes in [
            vec!["runtime.rs".into()],
            vec!["absent".into()],
            vec!["runtime.rs".into(), "Cargo.toml".into()],
        ] {
            assert!(update_scopes(&worktree, "builder", &scopes, ScopeChange::Shrink).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        fs::write(worktree.join("pending"), "keep dirty changes").unwrap();
        assert!(update_scopes(
            &worktree,
            "builder",
            &["Cargo.toml".into()],
            ScopeChange::Shrink
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(worktree.join("pending")).unwrap();
        update_scopes(
            &worktree,
            "builder",
            &["Cargo.toml".into()],
            ScopeChange::Shrink,
        )
        .unwrap();
        assert_eq!(read_claim(&path).unwrap().scopes, vec!["runtime.rs"]);
        start(&repo, "version", &["Cargo.toml".into()]).unwrap();
        assert_eq!(
            git(&worktree, &["show", "HEAD:runtime.rs"]).unwrap(),
            "changed"
        );
    }
    #[test]
    fn reservation_is_exclusive_and_lock_is_released() {
        let scratch = Scratch::new();
        let lock = Lock::acquire(&scratch.0).unwrap();
        assert!(Lock::acquire(&scratch.0).is_err());
        drop(lock);
        assert!(Lock::acquire(&scratch.0).is_ok());
    }
    #[test]
    fn local_worktrees_reject_overlap_and_preserve_dirty_work() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let first = start(&repo, "one", &["src".into()]).unwrap();
        assert!(first.join("target").is_dir());
        assert!(start(&repo, "two", &["SRC/cli".into()]).is_err());
        let second = start(&repo, "two", &["tests".into()]).unwrap();
        assert_ne!(first, second);
        fs::write(first.join("dirty.rs"), "preserve me").unwrap();
        assert!(release(&repo, "one").is_err());
        assert_eq!(
            fs::read_to_string(first.join("dirty.rs")).unwrap(),
            "preserve me"
        );
        release(&repo, "two").unwrap();
        assert!(second.exists());
        assert!(git(
            &repo,
            &["rev-parse", "--verify", "refs/heads/local-agent/two"]
        )
        .is_ok());
        // A clean, released, registered worktree is reused in place.
        assert_eq!(start(&repo, "two", &["tests".into()]).unwrap(), second);
        // The dirty, still-claimed one is never taken over.
        assert!(start(&repo, "one", &["src".into()]).is_err());
        assert_eq!(
            fs::read_to_string(first.join("dirty.rs")).unwrap(),
            "preserve me"
        );
    }
    #[test]
    fn parallel_scope_claims_have_one_winner() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["left", "right"]
            .into_iter()
            .map(|agent| {
                let repo = repo.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    start(&repo, agent, &["src".into()]).is_ok()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_claim_fails_closed() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let state = state(&repo).unwrap();
        fs::create_dir_all(state.join("claims")).unwrap();
        fs::write(state.join("claims/broken.claim"), "bad").unwrap();
        assert!(start(&repo, "one", &["src".into()]).is_err());
    }

    #[test]
    fn commit_rejects_wrong_worktree_and_outside_scope_without_staging() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "one", &["src".into()]).unwrap();
        fs::create_dir(worktree.join("src")).unwrap();
        fs::write(worktree.join("src/allowed.rs"), "allowed").unwrap();
        assert!(commit(&repo, "one", "wrong owner", None).is_err());
        fs::write(worktree.join("outside.rs"), "outside").unwrap();
        assert!(commit(&worktree, "one", "reject", None).is_err());
        assert!(git(&worktree, &["diff", "--cached", "--name-only"])
            .unwrap()
            .is_empty());
        fs::remove_file(worktree.join("outside.rs")).unwrap();
        // A staged delta cancelled by an unstaged edit still must be inspected.
        fs::write(worktree.join(".gitignore"), "changed\n").unwrap();
        git(&worktree, &["add", ".gitignore"]).unwrap();
        fs::write(worktree.join(".gitignore"), "/target/\n").unwrap();
        assert!(commit(&worktree, "one", "reject staged outside", None).is_err());
    }

    #[test]
    fn local_commit_suppresses_only_its_own_hooks_and_preserves_shared_setting() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "one", &["src".into()]).unwrap();
        git(&repo, &["config", "user.name", "Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        let hooks = scratch.0.join("failing-hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&repo, &["config", "core.hooksPath", text(&hooks).unwrap()]).unwrap();
        fs::create_dir(worktree.join("src")).unwrap();
        fs::write(worktree.join("src/a space.rs"), "allowed").unwrap();
        git(&worktree, &["add", "--", "src/a space.rs"]).unwrap();
        assert!(git(&worktree, &["commit", "-m", "blocked by hook"]).is_err());
        commit(&worktree, "one", "local change", None).unwrap();
        assert!(git(&worktree, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
        assert_eq!(
            git(&repo, &["config", "--get", "core.hooksPath"]).unwrap(),
            text(&hooks).unwrap()
        );
        assert!(git(&worktree, &["log", "-1", "--format=%B"])
            .unwrap()
            .contains("Task: LOCAL-one"));
    }

    #[test]
    fn signing_failure_preserves_the_previous_head_and_unsigned_retry() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = start(&repo, "signer", &["owned.rs".into()]).unwrap();
        let original = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        fs::write(worktree.join("owned.rs"), "scoped change").unwrap();
        git(
            &repo,
            &[
                "config",
                "gpg.program",
                text(&scratch.0.join("missing-signer")).unwrap(),
            ],
        )
        .unwrap();
        for key in ["ABC123", "0000000000000000000000000000000000000000"] {
            assert!(commit(&worktree, "signer", "sign", Some(key)).is_err());
            assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), original);
        }
        commit(&worktree, "signer", "unsigned retry", None).unwrap();
        assert_eq!(
            git(&worktree, &["show", "HEAD:owned.rs"]).unwrap(),
            "scoped change"
        );
    }

    fn worker(repo: &Path, agent: &str, file: &str, content: &str) -> PathBuf {
        let worktree = start(repo, agent, &[file.into()]).unwrap();
        fs::write(worktree.join(file), content).unwrap();
        commit(&worktree, agent, "fixture change", None).unwrap();
        worktree
    }

    #[test]
    fn batch_retries_all_claims_and_publishes_only_the_verified_combination() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let workers = ["left", "right"];
        for name in workers {
            worker(&repo, name, &format!("{name}.rs"), name);
        }
        let receipts = "refs/susi/integration/receipts/";
        assert!(integrate_with(&repo, &workers, |candidate, _| {
            assert!(candidate.join("left.rs").is_file());
            assert!(candidate.join("right.rs").is_file());
            Err("combined fixture rejected".into())
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        assert!(git(&repo, &["for-each-ref", receipts]).unwrap().is_empty());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
        let head = integrate_with(&repo, &workers, |candidate, _| {
            for name in workers {
                assert_eq!(
                    fs::read_to_string(candidate.join(format!("{name}.rs")))?,
                    name
                );
            }
            Ok(vec![
                "test-only combined fixture => exit code: 0; both files verified".into(),
            ])
        })
        .unwrap();
        let tree = git(&repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
        for name in workers {
            let source = git(&repo, &["rev-parse", &format!("local-agent/{name}")]).unwrap();
            let reference = format!("{receipts}{name}/{source}");
            let receipt = git(&repo, &["cat-file", "blob", &reference]).unwrap();
            assert!(receipt_matches(&repo, &reference, &receipt, &head, &tree).unwrap());
        }
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn integration_tests_the_combined_tree_and_publishes_atomic_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let original = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let left = worker(&repo, "left", "left.rs", "left");
        let right = worker(&repo, "right", "right.rs", "right");
        let source = git(&right, &["rev-parse", "HEAD"]).unwrap();
        fs::write(repo.join("primary-dirty.txt"), "preserve primary").unwrap();
        let mut first_candidate = PathBuf::new();
        let first = integrate_with(&repo, &["left"], |candidate, _| {
            first_candidate = candidate.to_path_buf();
            let mut build_output = candidate.join("target");
            for _ in 0..6 {
                build_output.push("nested-windows-artifact-directory-with-a-long-name");
            }
            fs::create_dir_all(&build_output)?;
            fs::write(build_output.join("probe"), "build output")?;
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert!(!candidate.join("right.rs").exists());
            Ok(vec![
                "test-only content verification passed => exit code: 0; fixture verifier completed"
                    .into(),
            ])
        })
        .unwrap();
        assert!(
            !first_candidate.exists(),
            "accepted scratch tree was retained"
        );
        let second = integrate_with(&repo, &["right"], |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
            Ok(vec!["test-only combined content verification passed => exit code: 0; fixture verifier completed".into()])
        })
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(integration_head(&repo).unwrap(), Some(second.clone()));
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), original);
        assert_eq!(
            fs::read_to_string(repo.join("primary-dirty.txt")).unwrap(),
            "preserve primary"
        );
        assert!(left.exists() && right.exists());
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        let receipt_ref = format!("refs/susi/integration/receipts/right/{source}");
        let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
        assert!(receipt.contains(&format!("commit={second}")));
        assert!(receipt.contains(&format!("previous={first}")));
        assert!(receipt.contains(&format!("source={source}")));
        let third = start(&repo, "third", &["third.rs".into()]).unwrap();
        assert_eq!(git(&third, &["rev-parse", "HEAD"]).unwrap(), second);
    }

    #[test]
    fn combined_integration_keeps_all_claims_on_failure_and_publishes_all_receipts() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let left = worker(&repo, "left", "left.rs", "left");
        // A dependent worker starts from the first worker's private source.
        let right = worker(&left, "right", "right.rs", "right");
        let left_source = git(&left, &["rev-parse", "HEAD"]).unwrap();
        let right_source = git(&right, &["rev-parse", "HEAD"]).unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            Err("deliberate gate failure".into())
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
        assert!(integrate_many_with(&repo, &["left", "left"], |_, _| {
            panic!("duplicate sources reached verification")
        })
        .is_err());
        let head = integrate_many_with(&repo, &["left", "right"], |candidate, _| {
            assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
            assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
            Ok(vec!["fixture combined verification => exit code: 0".into()])
        })
        .unwrap();
        for (name, source) in [("left", left_source), ("right", right_source)] {
            let receipt = git(
                &repo,
                &[
                    "cat-file",
                    "blob",
                    &format!("refs/susi/integration/receipts/{name}/{source}"),
                ],
            )
            .unwrap();
            assert!(receipt.contains(&format!("commit={head}")));
            assert!(receipt.contains(&format!("source={source}")));
        }
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn combined_integration_cannot_mutually_admit_unaccepted_bases() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        worker(&repo, "anchor", "anchor.rs", "anchor");
        fs::write(repo.join("unowned.rs"), "not accepted").unwrap();
        git(&repo, &["add", "unowned.rs"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "unowned base"],
        )
        .unwrap();
        worker(&repo, "left", "left.rs", "left");
        worker(&repo, "right", "right.rs", "right");
        let accepted = integrate_with(&repo, &["anchor"], |_, _| {
            Ok(vec!["fixture verification => exit code: 0".into()])
        })
        .unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            panic!("unaccepted shared base reached verification")
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(accepted));
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    }

    #[test]
    fn combined_integration_rechecks_every_source_and_scope() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        worker(&repo, "left", "left.rs", "left");
        let right = worker(&repo, "right", "right.rs", "right");
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            fs::write(right.join("right.rs"), "advanced")?;
            commit(&right, "right", "advance during gate", None)?;
            Ok(vec!["fixture verification => exit code: 0".into()])
        })
        .is_err());
        assert!(integration_head(&repo).unwrap().is_none());
        fs::write(right.join("outside.rs"), "unowned").unwrap();
        git(&right, &["add", "outside.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
        )
        .unwrap();
        assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
            panic!("unowned source reached verification")
        })
        .is_err());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    }

    #[test]
    fn accepted_imports_do_not_expand_the_workers_reserved_scope() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let right = worker(&repo, "right", "right.rs", "right");
        worker(&repo, "left", "left.rs", "left");
        let accepted = integrate_with(&repo, &["left"], |_, _| {
            Ok(vec![
                "fixture verification => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
        )
        .unwrap();
        let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
        assert!(
            check_source(&repo, &claim).is_ok(),
            "previously accepted left.rs is not new worker work"
        );
        fs::write(right.join("outside.rs"), "new unauthorized work").unwrap();
        git(&right, &["add", "outside.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
        )
        .unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("outside reserved scope"));
        // Even an accepted import cannot authorize a later edit to its file.
        git(&right, &["revert", "--no-edit", "HEAD"]).unwrap();
        fs::write(right.join("left.rs"), "tampered accepted import").unwrap();
        git(&right, &["add", "left.rs"]).unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "commit", "-qm", "tamper"],
        )
        .unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("outside reserved scope"));
    }

    #[test]
    fn scope_exemption_requires_related_accepted_history_and_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let right = worker(&repo, "right", "right.rs", "right");
        worker(&repo, "left", "left.rs", "left");
        let accepted = integrate_with(&repo, &["left"], |_, _| {
            Ok(vec![
                "fixture verification => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .unwrap();
        git(
            &right,
            &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
        )
        .unwrap();
        let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
        let receipts = git(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/integration/receipts/",
            ],
        )
        .unwrap();
        for receipt in receipts.lines() {
            let raw = git(&repo, &["cat-file", "blob", receipt]).unwrap();
            let tree = git(&repo, &["rev-parse", &format!("{accepted}^{{tree}}")]).unwrap();
            assert!(receipt_matches(&repo, receipt, &raw, &accepted, &tree).unwrap());
            for malformed in [
                format!("susi-local-integration-v1\ncommit={accepted}\ntree={tree}\n"),
                format!("{raw}\ncommit={accepted}\n"),
                raw.replace(
                    "fixture verification => exit code: 0; fixture verifier completed",
                    "hello",
                ),
                raw.replace("agent=left", "agent=other"),
                raw.replace(
                    "fixture verification",
                    "cargo test => exit code: 1; elapsed-ms=1",
                ),
            ] {
                assert!(!receipt_matches(&repo, receipt, &malformed, &accepted, &tree).unwrap());
            }
            git(&repo, &["update-ref", "-d", receipt]).unwrap();
        }
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("matching durable"));
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated history"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
        assert!(check_source(&repo, &claim)
            .unwrap_err()
            .to_string()
            .contains("does not descend"));
    }

    #[test]
    fn release_accepts_work_the_publisher_landed_on_fetched_main() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "pub", "pub.rs", "published");
        let head = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        // Not integrated and not on the fetched main: the reservation stays.
        let error = release(&repo, "pub").unwrap_err();
        assert!(
            error.to_string().contains("reservation retained"),
            "{error}"
        );
        // `local-snapshot submit` published it; a fetch mirrors that main.
        let main = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let tree = git(&worktree, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let merged = git(
            &repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &main,
                "-p",
                &head,
                "-m",
                "submission",
            ],
        )
        .unwrap();
        git(&repo, &["update-ref", FETCHED_MAIN_REF, &merged]).unwrap();
        release(&repo, "pub").unwrap();
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        // The next claim starts from that published main (it descends from
        // the stale integration ref) and reuses the worktree, whose branch
        // holds nothing the published main lacks.
        git(&repo, &["update-ref", INTEGRATION_REF, &main]).unwrap();
        let again = start(&repo, "pub", &["pub.rs".to_owned()]).unwrap();
        assert_eq!(again, worktree);
        assert_eq!(git(&again, &["rev-parse", "HEAD"]).unwrap(), merged);
        // Diverged refs keep the integration ref as the base.
        release(&repo, "pub").unwrap();
        let side = git(&repo, &["commit-tree", &tree, "-p", &main, "-m", "side"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &side]).unwrap();
        assert_eq!(claim_base(&repo).unwrap(), side);
    }

    #[test]
    fn failed_gate_retains_source_claim_and_previous_integration() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(release(&repo, "one").is_err());
        let result = integrate_with(&repo, &["one"], |_, attempt| {
            fs::write(attempt.join("gate-0.log"), "test failure evidence")?;
            Err("deliberate test-only gate failure".into())
        });
        assert!(result.is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
        assert_eq!(fs::read_to_string(worktree.join("one.rs")).unwrap(), "one");
        let attempts = state(&repo).unwrap().join("attempts");
        assert!(fs::read_dir(attempts).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .join("status")
            .is_file()));
    }

    #[test]
    fn integration_rechecks_committed_scope_and_candidate_mutation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(integrate_with(&repo, &["one"], |candidate, _| {
            fs::write(candidate.join("one.rs"), "modified after verification")?;
            Ok(vec![
                "test-only check => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .is_err());
        fs::write(worktree.join("outside.rs"), "escape").unwrap();
        git(&worktree, &["add", "outside.rs"]).unwrap();
        git(
            &worktree,
            &["-c", "core.hooksPath=", "commit", "-qm", "outside scope"],
        )
        .unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| panic!(
            "scope violation reached gate"
        ))
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    fn integration_cas_refuses_external_advancement_without_publishing_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        let source = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
        let external = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| {
            git(&repo, &["update-ref", INTEGRATION_REF, &external])?;
            Ok(vec![
                "test-only check => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(external));
        assert!(git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/susi/integration/receipts/one/{source}")
            ]
        )
        .is_err());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn source_advancement_during_gate_cannot_be_promoted_as_tested() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let worktree = worker(&repo, "one", "one.rs", "one");
        assert!(integrate_with(&repo, &["one"], |_, _| {
            fs::write(worktree.join("one.rs"), "new untested source")?;
            commit(&worktree, "one", "advanced during verification", None)?;
            Ok(vec![
                "test-only prior-head check => exit code: 0; fixture verifier completed".into(),
            ])
        })
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), None);
    }

    #[test]
    // Queue this long real qualification early; preserve every assertion below.
    fn a_critical_path_real_native_cargo_gates_accept_a_passing_fixture() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let native_worker = start(
            &repo,
            "native",
            &[
                "Cargo.toml".into(),
                "Cargo.lock".into(),
                "deny.toml".into(),
                "src".into(),
                ".agents/tasks/T-FIXTURE-1.json".into(),
            ],
        )
        .unwrap();
        fs::create_dir(native_worker.join("src")).unwrap();
        fs::write(native_worker.join("Cargo.toml"), "[package]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n") .unwrap();
        fs::write(
            native_worker.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            native_worker.join("deny.toml"),
            "[licenses]\nallow = [\"MIT\"]\n",
        )
        .unwrap();
        fs::write(
            native_worker.join("src/lib.rs"),
            "#[test]\nfn actual_test() {\n    assert_eq!(2 + 2, 4);\n}\n",
        )
        .unwrap();
        fs::create_dir_all(native_worker.join(".agents/tasks")).unwrap();
        fs::write(native_worker.join(".agents/tasks/T-FIXTURE-1.json"),
            serde_json::to_vec(&serde_json::json!({"id":"T-FIXTURE-1","native_worker":"native",
                "status":"in_progress","scopes":["Cargo.toml","Cargo.lock","deny.toml","src",".agents/tasks/T-FIXTURE-1.json"],
                "accept":{"cmd":["cargo","test","--offline","--locked"]}})).unwrap()).unwrap();
        commit(&native_worker, "native", "native gate fixture", None).unwrap();
        let second = start(
            &repo,
            "second",
            &["tests".into(), ".agents/tasks/T-SECOND-1.json".into()],
        )
        .unwrap();
        fs::create_dir_all(second.join("tests")).unwrap();
        fs::create_dir_all(second.join(".agents/tasks")).unwrap();
        fs::write(
            second.join("tests/second.rs"),
            "#[test]\nfn second_actual_test() {\n    assert_eq!(3 + 3, 6);\n}\n",
        )
        .unwrap();
        fs::write(second.join(".agents/tasks/T-SECOND-1.json"),serde_json::to_vec(&serde_json::json!({
            "id":"T-SECOND-1","native_worker":"second","status":"in_progress",
            "scopes":["tests",".agents/tasks/T-SECOND-1.json"],"accept":{"cmd":["cargo","test","--offline","--locked"]}
        })).unwrap()).unwrap();
        commit(&second, "second", "second native gate fixture", None).unwrap();
        let head = integrate_many_with(&repo, &["native", "second"], verify_candidate).unwrap();
        assert_eq!(integration_head(&repo).unwrap(), Some(head.clone()));
        verify_real_native_provenance(&repo, &scratch.0, &head);
        let broken = worker(
            &repo,
            "broken",
            "src/lib.rs",
            "#[test]\nfn failing_test() {\n    let actual = std::hint::black_box(4);\n    assert_eq!(actual, 5);\n}\n",
        );
        extend(&broken, "broken", &[".agents/tasks/T-BROKEN-1.json".into()]).unwrap();
        fs::write(
            broken.join(".agents/tasks/T-BROKEN-1.json"),
            serde_json::to_vec(
                &serde_json::json!({"id":"T-BROKEN-1","native_worker":"broken",
                "status":"in_progress","scopes":["src/lib.rs",".agents/tasks/T-BROKEN-1.json"],
                "accept":{"cmd":["cargo","test","--offline","--locked"]}}),
            )
            .unwrap(),
        )
        .unwrap();
        commit(&broken, "broken", "bind failing fixture acceptance", None).unwrap();
        let failure = integrate_with(&repo, &["broken"], verify_candidate).unwrap_err();
        eprintln!("failing fixture gate: {failure}");
        assert_eq!(integration_head(&repo).unwrap(), Some(head));
        assert!(broken.exists());
        let attempts = state(&repo).unwrap().join("attempts");
        let failed_blob = fs::read_dir(attempts)
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                fs::read_to_string(path.join("gate-3.log"))
                    .is_ok_and(|log| log.contains("test result: FAILED"))
                    .then(|| {
                        fs::read_to_string(path.join("gate-3.log.oid"))
                            .unwrap()
                            .trim()
                            .to_owned()
                    })
            })
            .expect("the genuine failing gate retains a content-addressed log");
        git(&repo, &["gc", "--prune=now"]).unwrap();
        assert!(git(&repo, &["cat-file", "blob", &failed_blob])
            .unwrap()
            .contains("test result: FAILED"));
    }

    fn verify_real_native_provenance(repo: &Path, scratch: &Path, head: &str) {
        let base = git(repo, &["rev-parse", "HEAD"]).unwrap();
        let base_tree = git(repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let accepted_tree = git(repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
        let diagnostics =
            b"test fixture: historical ownership incomplete; forward acceptance only\n";
        let receipts: Vec<_> = git(
            repo,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/susi/provenance/receipts/",
            ],
        )
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
        assert_eq!(receipts.len(), 2);
        let verifier = native_provenance::file_digest(&env::current_exe().unwrap()).unwrap();
        let policy = native_provenance::policy_digest();
        let permit = serde_json::json!({"schema":native_provenance::PERMIT_SCHEMA,
            "verifier_sha256":verifier,"policy_sha256":policy,
            "source":{"commit":head,"tree":accepted_tree},
            "baseline":{"commit":base,"tree":base_tree,"historical_ownership":"incomplete",
                "diagnostics_blob":native_provenance::store_blob(repo,diagnostics).unwrap(),
                "diagnostics_sha256":native_provenance::digest(diagnostics)},"receipts":receipts});
        let path = scratch.join("independent-permit.json");
        let check = |value: &serde_json::Value, expected_head: &str| {
            let bytes = serde_json::to_vec(value).unwrap();
            fs::write(&path, &bytes).unwrap();
            native_provenance::verify(
                repo,
                &path,
                &native_provenance::TrustPins {
                    permit_sha256: &native_provenance::digest(&bytes),
                    verifier_sha256: &verifier,
                    policy_sha256: &policy,
                },
                expected_head,
            )
        };
        let report = check(&permit, head).unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["historical_ownership"], "incomplete");
        let failed_bytes = b"failed-attempt evidence not present in any accepted receipt\n";
        let failed_blob = native_provenance::store_blob(repo, failed_bytes).unwrap();
        assert_eq!(
            native_provenance::store_blob(repo, failed_bytes).unwrap(),
            failed_blob
        );
        let failed_reference = format!("refs/susi/evidence/objects/{failed_blob}");
        let missing_target = "refs/susi/evidence/missing-fixture-target";
        git(repo, &["symbolic-ref", &failed_reference, missing_target]).unwrap();
        assert!(native_provenance::store_blob(repo, failed_bytes).is_err());
        assert_eq!(
            git(repo, &["symbolic-ref", "--quiet", &failed_reference]).unwrap(),
            missing_target
        );
        git(repo, &["symbolic-ref", "--delete", &failed_reference]).unwrap();
        assert_eq!(
            native_provenance::store_blob(repo, failed_bytes).unwrap(),
            failed_blob
        );
        git(repo, &["repack", "-ad"]).unwrap();
        git(repo, &["gc", "--prune=now"]).unwrap();
        assert_eq!(check(&permit, head).unwrap()["verified"], true);
        assert_eq!(
            git(repo, &["cat-file", "blob", &failed_blob]).unwrap(),
            String::from_utf8_lossy(failed_bytes).trim()
        );
        let anchored: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
        let claim_oid = anchored["claim"]["blob"].as_str().unwrap();
        let claim_reference = format!("refs/susi/evidence/objects/{claim_oid}");
        let claim_bytes = git(repo, &["cat-file", "blob", claim_oid]).unwrap() + "\n";
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                "-d",
                &claim_reference,
                claim_oid,
            ],
        )
        .unwrap();
        assert!(check(&permit, head).is_err());
        assert_eq!(
            native_provenance::store_blob(repo, claim_bytes.as_bytes()).unwrap(),
            claim_oid
        );
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                &claim_reference,
                &failed_blob,
                claim_oid,
            ],
        )
        .unwrap();
        assert!(native_provenance::store_blob(repo, claim_bytes.as_bytes()).is_err());
        assert!(check(&permit, head).is_err());
        git(
            repo,
            &[
                "update-ref",
                "--no-deref",
                &claim_reference,
                claim_oid,
                &failed_blob,
            ],
        )
        .unwrap();
        assert_eq!(check(&permit, head).unwrap()["verified"], true);
        // Candidate replacement refs cannot reinterpret independently pinned IDs.
        git(repo, &["replace", head, &base]).unwrap();
        assert_eq!(
            check(&permit, head).unwrap()["source"]["tree"],
            accepted_tree
        );
        let mut replaced_tree = permit.clone();
        replaced_tree["source"]["tree"] = base_tree.clone().into();
        assert!(check(&replaced_tree, head).is_err());
        git(repo, &["replace", "-d", head]).unwrap();
        let graft = repo.join(git(repo, &["rev-parse", "--git-path", "info/grafts"]).unwrap());
        fs::create_dir_all(graft.parent().unwrap()).unwrap();
        fs::write(&graft, format!("{head} {base}\n")).unwrap();
        assert!(check(&permit, head).is_err());
        fs::remove_file(graft).unwrap();
        assert!(check(&permit, &base).is_err());
        let original: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
        let reject_receipt = |changed: serde_json::Value| {
            let mut altered = permit.clone();
            altered["receipts"][0] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&changed).unwrap())
                    .unwrap()
                    .into();
            assert!(check(&altered, head).is_err());
        };
        let mut changed = original.clone();
        changed["source"]["tree"] = base_tree.clone().into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["claim"]["scopes"] = serde_json::json!(["outside"]);
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][0]["exit_code"] = 1.into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][3]["passed"] = 0.into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][0]["log_sha256"] = "0".repeat(64).into();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gates"][3]["argv"]
            .as_array_mut()
            .unwrap()
            .extend(["--exclude".into(), "susi-core".into()]);
        changed["gate_plan"][3]["argv"] = changed["gates"][3]["argv"].clone();
        reject_receipt(changed);
        let mut changed = original.clone();
        changed["gate_plan"].as_array_mut().unwrap().remove(1);
        changed["gates"].as_array_mut().unwrap().remove(1);
        reject_receipt(changed);
        let mut duplicate = permit.clone();
        duplicate["receipts"]
            .as_array_mut()
            .unwrap()
            .push(receipts[0].clone().into());
        assert!(check(&duplicate, head).is_err());
        // A same-tree merge can hide an unrelated second parent: ancestry, not
        // final net diff, must still reject its unowned source commit.
        let rogue = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &base_tree,
                "-p",
                &base,
            ],
            b"unowned source\n",
        )
        .unwrap();
        let hidden = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &accepted_tree,
                "-p",
                head,
                "-p",
                &rogue,
            ],
            b"same-tree uncovered second parent\n",
        )
        .unwrap();
        let mut hidden_permit = permit.clone();
        hidden_permit["source"]["commit"] = hidden.clone().into();
        for (index, oid) in receipts.iter().enumerate() {
            let mut record: serde_json::Value =
                serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
            record["integration"]["commit"] = hidden.clone().into();
            hidden_permit["receipts"][index] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                    .unwrap()
                    .into();
        }
        assert!(check(&hidden_permit, &hidden).is_err());
        // A source merge may not claim unrelated pre-reservation history whose
        // out-of-scope edit was subsequently reverted to a clean net diff.
        let outside_blob =
            native_provenance::store_blob(repo, b"unowned historical edit\n").unwrap();
        let outside_tree = git_input(
            repo,
            &["mktree"],
            format!(
                "{}\n100644 blob {outside_blob}\toutside.txt\n",
                git(repo, &["ls-tree", &base]).unwrap()
            )
            .as_bytes(),
        )
        .unwrap();
        let outside_commit = git_input(
            repo,
            &["-c", "commit.gpgsign=false", "commit-tree", &outside_tree],
            b"unrelated scope bypass\n",
        )
        .unwrap();
        let reverted = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &base_tree,
                "-p",
                &outside_commit,
            ],
            b"hide unrelated edit\n",
        )
        .unwrap();
        let original_source = original["source"]["commit"].as_str().unwrap();
        let original_tree = original["source"]["tree"].as_str().unwrap();
        let mixed_source = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                original_tree,
                "-p",
                original_source,
                "-p",
                &reverted,
            ],
            b"source with hidden pre-scope history\n",
        )
        .unwrap();
        let mixed_head = git_input(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &accepted_tree,
                "-p",
                head,
                "-p",
                &mixed_source,
            ],
            b"integrated hidden pre-scope history\n",
        )
        .unwrap();
        let mut mixed_permit = permit.clone();
        mixed_permit["source"]["commit"] = mixed_head.clone().into();
        for (index, oid) in receipts.iter().enumerate() {
            let mut record: serde_json::Value =
                serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
            record["integration"]["commit"] = mixed_head.clone().into();
            if index == 0 {
                record["source"]["commit"] = mixed_source.clone().into();
            }
            mixed_permit["receipts"][index] =
                native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                    .unwrap()
                    .into();
        }
        assert!(check(&mixed_permit, &mixed_head).is_err());
        let original_bytes = serde_json::to_vec(&permit).unwrap();
        let original_digest = native_provenance::digest(&original_bytes);
        fs::write(&path, b"forged permit").unwrap();
        assert!(native_provenance::verify(
            repo,
            &path,
            &native_provenance::TrustPins {
                permit_sha256: &original_digest,
                verifier_sha256: &verifier,
                policy_sha256: &policy
            },
            head
        )
        .is_err());
        let candidate_permit = repo.join("self-authorized.json");
        fs::write(&candidate_permit, &original_bytes).unwrap();
        assert!(native_provenance::verify(
            repo,
            &candidate_permit,
            &native_provenance::TrustPins {
                permit_sha256: &original_digest,
                verifier_sha256: &verifier,
                policy_sha256: &policy
            },
            head
        )
        .is_err());
        fs::remove_file(candidate_permit).unwrap();
    }

    #[test]
    fn merge_conflict_preserves_candidate_and_reservation() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        fs::write(repo.join("common.rs"), "original\n").unwrap();
        git(&repo, &["add", "common.rs"]).unwrap();
        git(&repo, &["-c", "core.hooksPath=", "commit", "-qm", "base"]).unwrap();
        worker(&repo, "one", "common.rs", "worker\n");
        fs::write(repo.join("common.rs"), "integrated\n").unwrap();
        git(&repo, &["add", "common.rs"]).unwrap();
        git(
            &repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "other change"],
        )
        .unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &head]).unwrap();
        // This fixture models a previously accepted conflicting change. Supply
        // its matching receipt so the test reaches the actual merge conflict.
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let receipt = format!(
            "susi-local-integration-v1\nagent=fixture\nsource={head}\nclaim-base={head}\nprevious=none\ncommit={head}\ntree={tree}\ntest-only conflict fixture => exit code: 0; fixture verified\n"
        );
        let blob = git_input(&repo, &["hash-object", "-w", "--stdin"], receipt.as_bytes()).unwrap();
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/susi/integration/receipts/fixture/{head}"),
                &blob,
            ],
        )
        .unwrap();
        assert!(integrate_with(&repo, &["one"], |_, _| panic!(
            "conflict reached verification"
        ))
        .is_err());
        assert_eq!(integration_head(&repo).unwrap(), Some(head));
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
        let worktrees = git(&repo, &["worktree", "list", "--porcelain"]).unwrap();
        let candidate = worktrees
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .find(|path| path.ends_with("-verify-candidate"))
            .unwrap();
        assert!(!git(
            Path::new(candidate),
            &["diff", "--name-only", "--diff-filter=U"]
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn disk_full_signatures_are_recognized_in_gate_logs() {
        for log in [
            "error: failed to write: No space left on device (os error 28)",
            "LLVM ERROR: IO failure on output stream: No space left on device",
            "There is not enough space on the disk. (os error 112)",
        ] {
            assert!(gate_out_of_space(log.as_bytes()), "{log}");
        }
        for log in [
            "",
            "error[E0308]: mismatched types",
            "test result: FAILED. 1 passed; 1 failed",
            "No such file or directory (os error 2)",
        ] {
            assert!(!gate_out_of_space(log.as_bytes()), "{log}");
        }
    }

    #[test]
    fn disk_floor_follows_the_workspace_the_gate_and_the_warm_target() {
        let susi = Some(RepoKind::Susi);
        let cold = gate_disk_floor(susi, true, false);
        let warm = gate_disk_floor(susi, true, true);
        let narrowed_cold = gate_disk_floor(susi, false, false);
        let narrowed_warm = gate_disk_floor(susi, false, true);
        assert!(cold > warm, "{cold} {warm}");
        assert!(warm > narrowed_cold, "{warm} {narrowed_cold}");
        assert_eq!(narrowed_cold, narrowed_warm);
        assert!(narrowed_warm >= GIB);
        let commercial = Some(RepoKind::Commercial);
        assert!(gate_disk_floor(commercial, true, false) < cold);
        assert!(gate_disk_floor(commercial, true, false) > gate_disk_floor(commercial, true, true));
        assert_eq!(
            gate_disk_floor(commercial, true, true),
            gate_disk_floor(commercial, false, false)
        );
        // A fixture workspace only needs room to compile at all.
        let fixture = gate_disk_floor(None, true, false);
        assert!(
            fixture < gate_disk_floor(commercial, true, true),
            "{fixture}"
        );
        assert!(fixture > 0);
    }

    #[test]
    fn disk_free_space_is_measured_for_existing_and_future_paths() {
        let scratch = Scratch::new();
        assert!(free_space(&scratch.0).is_some_and(|free| free > 0));
        assert!(free_space(&scratch.0.join("not").join("yet")).is_some());
        assert_eq!(gib(3 * GIB / 2), "1.5 GiB");
    }

    #[test]
    fn dropped_locks_release_while_a_descriptor_copy_is_alive() {
        let scratch = Scratch::new();
        let gate = acquire_host_gate(&scratch.0.join("host"), "test").unwrap();
        let gate_copy = gate._file.try_clone().unwrap();
        drop(gate);
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(scratch.0.join("host").join("gate.lock"))
            .unwrap();
        assert!(probe.try_lock().is_ok(), "host gate stayed held by a copy");
        release_probe(probe);

        let state = scratch.0.join("state");
        let lock = Lock::acquire(&state).unwrap();
        let lock_copy = lock._file.try_clone().unwrap();
        drop(lock);
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("lock.file"))
            .unwrap();
        assert!(
            probe.try_lock().is_ok(),
            "workflow lock stayed held by a copy"
        );
        release_probe(probe);
        drop((gate_copy, lock_copy));
    }

    #[test]
    fn disk_reclaim_frees_idle_caches_and_keeps_busy_and_current_ones() {
        let scratch = Scratch::new();
        let workflow = scratch.0.join("wf");
        let slots = workflow.join("slots");
        let seed = |dir: &Path| {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("unit.o"), vec![7u8; 2048]).unwrap();
        };
        seed(&workflow.join("integration-target"));
        seed(&slots.join("2").join("target"));
        seed(&slots.join("3").join("target"));
        seed(&slots.join("owner-notes"));
        let candidate = scratch.0.join("susi-verify-candidate-1");
        seed(&candidate.join("target"));
        seed(&scratch.0.join("susi-verify-candidate-2").join("target"));
        seed(&scratch.0.join("susi-verify-candidate-3").join("target"));
        // Slot 3 is a running ship's: its lock is held.
        let busy = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(slots.join("3.lock"))
            .unwrap();
        busy.try_lock().unwrap();
        let busy = HostGate { _file: busy };
        let freed = reclaim_gate_caches(&workflow, &candidate);
        let mut paths: Vec<_> = freed.iter().map(|(path, _)| path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                scratch.0.join("susi-verify-candidate-2").join("target"),
                workflow.join("integration-target"),
                slots.join("2").join("target"),
            ]
        );
        assert!(freed.iter().all(|(_, size)| *size == 2048), "{freed:?}");
        assert!(
            candidate.join("target").is_dir(),
            "the current candidate keeps its target"
        );
        assert!(
            slots.join("3").join("target").is_dir(),
            "a busy slot keeps its stash"
        );
        assert!(
            scratch
                .0
                .join("susi-verify-candidate-3")
                .join("target")
                .is_dir(),
            "a busy slot keeps its candidate target"
        );
        assert!(
            slots.join("owner-notes").is_dir(),
            "only numbered slots are caches"
        );
        // Once that ship is gone, its caches are idle too; then nothing is left.
        drop(busy);
        let mut released: Vec<_> = reclaim_gate_caches(&workflow, &candidate)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        released.sort();
        assert_eq!(
            released,
            vec![
                scratch.0.join("susi-verify-candidate-3").join("target"),
                slots.join("3").join("target"),
            ]
        );
        assert!(reclaim_gate_caches(&workflow, &candidate).is_empty());
    }

    struct FakeRemote {
        mains: Vec<String>,
        fetches: usize,
        pushes: Vec<(String, String)>,
        fail_push: bool,
    }

    impl FakeRemote {
        fn single(main: String) -> Self {
            Self {
                mains: vec![main],
                fetches: 0,
                pushes: Vec::new(),
                fail_push: false,
            }
        }
    }

    impl Remote for FakeRemote {
        fn fetch_main(&mut self, _root: &Path) -> Result<String> {
            let sha = self
                .mains
                .get(self.fetches)
                .or_else(|| self.mains.last())
                .cloned()
                .ok_or("no origin/main")?;
            self.fetches += 1;
            Ok(sha)
        }
        fn push_ref(&mut self, _root: &Path, sha: &str, git_ref: &str) -> Result<()> {
            require_ship_ref(git_ref)?;
            if self.fail_push {
                return Err("push refused".into());
            }
            self.pushes.push((sha.to_owned(), git_ref.to_owned()));
            Ok(())
        }
    }

    fn local_gate_evidence() -> Vec<String> {
        vec![
            "cargo fmt --all --check => exit code: 0; elapsed-ms=1".into(),
            "cargo deny --offline --locked check licenses => exit code: 0; elapsed-ms=1".into(),
            "cargo clippy --workspace --all-targets --offline --locked => exit code: 0; elapsed-ms=1".into(),
            "cargo test --workspace --offline --locked => exit code: 0; elapsed-ms=1".into(),
        ]
    }

    fn ship_fixture(agent: &str) -> (Scratch, PathBuf, String) {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        worker(&repo, agent, "owned.rs", "shipped");
        (scratch, repo, base)
    }

    #[test]
    fn ship_publishes_when_local_integration_ref_is_unrelated() {
        let (_scratch, repo, base) = ship_fixture("one");
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
        let mut remote = FakeRemote::single(base);
        ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(!remote.pushes.is_empty());
        assert_eq!(
            git(&repo, &["rev-parse", "refs/susi/integration/displaced"]).unwrap(),
            unrelated
        );
    }

    /// Regression (2026-11-06): the first ship after displacing an unrelated
    /// integration ref wrote `previous=<displaced>`, so its own receipt never
    /// verified and every later ship failed at load with "no matching durable
    /// source/tree receipt". A second worker must ship on top of the first.
    #[test]
    fn ship_after_displacing_an_unrelated_ref_keeps_later_ships_working() {
        let (_scratch, repo, base) = ship_fixture("one");
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
        let mut remote = FakeRemote::single(base.clone());
        let first = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        // The receipts describe the verified merge; origin/main holds its
        // receipt commit.
        let first_head = first.merge.clone();
        let first_published = first.head.clone();
        let receipt_ref = git(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/integration/receipts/one/",
            ],
        )
        .unwrap();
        let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
        assert!(
            receipt.contains(&format!("previous={base}")),
            "a displaced ref must not be recorded as previous: {receipt}"
        );
        assert!(!receipt.contains(&format!("previous={unrelated}")));
        let first_tree = git(&repo, &["rev-parse", &format!("{first_head}^{{tree}}")]).unwrap();
        assert!(receipt_matches(&repo, &receipt_ref, &receipt, &first_head, &first_tree).unwrap());

        // Receipts written before the fix named the displaced ref; they must
        // still verify so repositories already in that state can ship again.
        let legacy = receipt.replace(
            &format!("previous={base}"),
            &format!("previous={unrelated}"),
        );
        assert!(receipt_matches(&repo, &receipt_ref, &legacy, &first_head, &first_tree).unwrap());
        let malformed = receipt.replace(&format!("previous={base}"), "previous=not-an-oid");
        assert!(
            !receipt_matches(&repo, &receipt_ref, &malformed, &first_head, &first_tree).unwrap()
        );

        worker(&repo, "two", "second.rs", "second");
        let mut remote = FakeRemote::single(first_published.clone());
        let second = ship(&repo, &["two"], &mut remote, |candidate, _attempt| {
            assert!(candidate.join("owned.rs").is_file());
            assert!(candidate.join("second.rs").is_file());
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(is_ancestor(&repo, &first_published, &second.merge).unwrap());
        assert_eq!(remote.pushes.last().unwrap().0, second.head);
    }

    #[test]
    fn host_gate_admits_one_holder_and_frees_on_drop() {
        let scratch = Scratch::new();
        let dir = scratch.repo().join("host-gate");
        let first = acquire_host_gate(&dir, "first").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("owner")).unwrap().trim(),
            "first"
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter_dir = dir.clone();
        let waiter = std::thread::spawn(move || {
            let gate = acquire_host_gate(&waiter_dir, "second").unwrap();
            sender.send(()).unwrap();
            drop(gate);
        });
        // The second holder must still be waiting while the first holds it.
        assert!(receiver
            .recv_timeout(std::time::Duration::from_millis(800))
            .is_err());
        drop(first);
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        waiter.join().unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("owner")).unwrap().trim(),
            "second"
        );
    }

    #[test]
    fn a_leftover_host_gate_file_does_not_block() {
        // A crashed holder leaves the lock file and owner behind; only a live
        // OS lock blocks, so the next gate proceeds at once.
        let scratch = Scratch::new();
        let dir = scratch.repo().join("host-gate");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("gate.lock"), "").unwrap();
        fs::write(dir.join("owner"), "pid=1 crashed\n").unwrap();
        let started = std::time::Instant::now();
        drop(acquire_host_gate(&dir, "next").unwrap());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn narrowed_xtask_gates_keep_release_tooling() {
        let mut gates = vec![
            vec!["fmt".to_owned(), "--all".to_owned()],
            ["clippy", "-p", "xtask", "--", "-D", "warnings"]
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect(),
            ["test", "-p", "xtask", "-p", "susi"]
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect(),
        ];
        let without: std::collections::BTreeSet<String> = ["susi".to_owned()].into();
        let mut untouched = gates.clone();
        include_release_tooling_for_xtask(&mut untouched, &without);
        assert_eq!(untouched, gates);
        let with: std::collections::BTreeSet<String> =
            ["susi".to_owned(), "xtask".to_owned()].into();
        include_release_tooling_for_xtask(&mut gates, &with);
        include_release_tooling_for_xtask(&mut gates, &with);
        assert_eq!(gates[0], ["fmt", "--all"]);
        assert_eq!(
            gates[1],
            [
                "clippy",
                "-p",
                "xtask",
                "--features",
                "xtask/release-tooling",
                "--",
                "-D",
                "warnings"
            ]
        );
        assert_eq!(
            gates[2],
            [
                "test",
                "-p",
                "xtask",
                "-p",
                "susi",
                "--features",
                "xtask/release-tooling"
            ]
        );
    }

    #[test]
    fn doc_gate_follows_the_test_gate_and_names_only_libraries() {
        let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let whole = strings(&[
            "nextest",
            "run",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
            "--features",
            "xtask/release-tooling",
        ]);
        assert_eq!(
            doc_gate(&whole, None).unwrap(),
            strings(&[
                "test",
                "--doc",
                "--workspace",
                "--offline",
                "--locked",
                "--target",
                "h",
                "--target-dir",
                "t",
                "--features",
                "xtask/release-tooling",
            ])
        );
        let narrowed = strings(&[
            "nextest",
            "run",
            "-p",
            "susi",
            "-p",
            "xtask",
            "-p",
            "susi-core",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
        ]);
        let libraries: BTreeSet<String> = ["susi".to_owned(), "susi-core".to_owned()].into();
        assert_eq!(
            doc_gate(&narrowed, Some(&libraries)).unwrap(),
            strings(&[
                "test",
                "--doc",
                "-p",
                "susi",
                "-p",
                "susi-core",
                "--offline",
                "--locked",
                "--target",
                "h",
                "--target-dir",
                "t",
            ])
        );
        let narrowed_xtask = strings(&[
            "nextest",
            "run",
            "-p",
            "susi",
            "-p",
            "xtask",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
            "--features",
            "xtask/release-tooling",
        ]);
        assert_eq!(
            doc_gate(&narrowed_xtask, Some(&libraries)).unwrap(),
            strings(&[
                "test",
                "--doc",
                "-p",
                "susi",
                "--offline",
                "--locked",
                "--target",
                "h",
                "--target-dir",
                "t",
            ])
        );
        let only_bins = strings(&["nextest", "run", "-p", "xtask", "--offline", "--locked"]);
        assert!(doc_gate(&only_bins, Some(&libraries)).is_none());
    }

    #[test]
    fn cargo_test_is_the_default_runner_and_nextest_is_opt_in() {
        let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let nextest_gate = strings(&["nextest", "run", "--workspace", "--offline", "--locked"]);
        let mut default = nextest_gate.clone();
        select_test_runner(&mut default, false);
        assert_eq!(
            default,
            strings(&["test", "--workspace", "--offline", "--locked"])
        );
        let mut opted_in = nextest_gate.clone();
        select_test_runner(&mut opted_in, true);
        assert_eq!(opted_in, nextest_gate);
        let mut already_test = strings(&["test", "-p", "x"]);
        select_test_runner(&mut already_test, false);
        assert_eq!(already_test, strings(&["test", "-p", "x"]));
    }

    #[test]
    fn ship_does_not_push_when_the_gate_fails() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Err("gate failed".into())
        })
        .unwrap_err();
        assert!(error.to_string().contains("gate failed"), "{error}");
        assert!(remote.pushes.is_empty());
    }

    #[test]
    fn ship_pushes_only_main_fast_forward() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(!remote.pushes.is_empty());
        for (_sha, git_ref) in &remote.pushes {
            assert_eq!(git_ref, "refs/heads/main");
        }
        assert!(require_ship_ref("refs/heads/other").is_err());
    }

    #[test]
    fn ship_publishes_the_receipt_commit_and_nothing_else() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base);
        let mut verified = String::new();
        let report = ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
            verified = git(candidate, &["rev-parse", "HEAD"])?;
            Ok(local_gate_evidence())
        })
        .unwrap();
        // The verified merge is published under its receipt commit, and the
        // push of that commit is the only thing the remote receives.
        assert_eq!(report.merge, verified);
        assert_ne!(report.head, verified);
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{}^1", report.head)]).unwrap(),
            verified
        );
        assert_eq!(
            remote.pushes,
            vec![(report.head.clone(), "refs/heads/main".to_owned())]
        );
    }

    /// Every landing publishes a receipt-only commit carrying
    /// `.agents/receipts/<verified head>.json`, recorded at
    /// `refs/susi/receipts/<verified head>`; without that record nothing is
    /// published (`ship --retry` included).
    /// The gate verifies the bytes its receipt names: a host whose Git
    /// converts line endings on checkout (`core.autocrlf=true`, the Git for
    /// Windows default) still gets the committed LF in its candidate.
    #[test]
    fn the_gate_candidate_holds_the_committed_bytes_under_autocrlf() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        worker(&repo, "one", "spec.mdx", "one\ntwo\n");
        git(&repo, &["config", "core.autocrlf", "true"]).unwrap();
        let mut remote = FakeRemote::single(base);
        let mut seen = Vec::new();
        ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
            seen = fs::read(candidate.join("spec.mdx"))?;
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert_eq!(seen, b"one\ntwo\n");
    }

    #[test]
    fn ship_batch_receipt_records_the_final_fetched_remote_not_an_intermediate_merge() {
        for moved_during_gate in [false, true] {
            let (_scratch, repo, base) = ship_fixture("one");
            worker(&repo, "two", "second.rs", "second");
            let expected = if moved_during_gate {
                commit_on(&repo, "remote-change.rs", "remote change")
            } else {
                base.clone()
            };
            let mut remote = FakeRemote::single(base.clone());
            if moved_during_gate {
                remote.mains = vec![base, expected.clone(), expected.clone()];
            }
            let runs = std::cell::Cell::new(0);
            let report = ship(
                &repo,
                &["one", "two"],
                &mut remote,
                |_candidate, _attempt| {
                    runs.set(runs.get() + 1);
                    Ok(local_gate_evidence())
                },
            )
            .unwrap();
            assert_eq!(runs.get(), if moved_during_gate { 2 } else { 1 });
            assert_eq!(remote.pushes.len(), 1);
            let parent = git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap();
            assert_ne!(
                parent, expected,
                "the second source creates an intermediate merge"
            );
            let receipt: serde_json::Value = serde_json::from_str(
                &git(
                    &repo,
                    &[
                        "show",
                        &format!("{}:{}", report.head, gate_receipt::path_for(&report.merge)),
                    ],
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["integration"]["origin_main"], expected);
            assert_eq!(receipt["integration"]["merge_first_parent"], parent);
            assert!(is_ancestor(&repo, &expected, &report.merge).unwrap());
            gate_receipt::check_digest(&receipt).unwrap();
        }
    }

    #[test]
    fn ship_publishes_the_gate_receipt_commit_and_refuses_without_one() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base.clone());
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        let verified = report.merge.clone();
        let published = report.head.clone();
        assert_eq!(
            git(&repo, &["rev-parse", &gate_receipt::ref_for(&verified)]).unwrap(),
            published
        );
        assert_eq!(
            git(&repo, &["rev-parse", INTEGRATION_REF]).unwrap(),
            verified
        );
        gate_receipt::check_receipt_only(&repo, &verified, &published).unwrap();
        let path = gate_receipt::path_for(&verified);
        let changed = git(
            &repo,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                &verified,
                &published,
            ],
        )
        .unwrap();
        assert_eq!(changed, path);
        let receipt: serde_json::Value =
            serde_json::from_str(&git(&repo, &["show", &format!("{published}:{path}")]).unwrap())
                .unwrap();
        gate_receipt::check_digest(&receipt).unwrap();
        assert_eq!(receipt["integration"]["commit"], verified);
        assert_eq!(receipt["integration"]["origin_main"], base);
        assert_eq!(receipt["sources"][0]["agent"], "one");
        assert_eq!(receipt["gates"].as_array().unwrap().len(), 4);
        assert!(receipt["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["exit_code"] == 0));
        let message = git(&repo, &["log", "-1", "--format=%B", &published]).unwrap();
        assert!(message.contains("Task: LOCAL-one"), "{message}");
        // No operator path leaks: the slot, the scratch repository, the home.
        let text = git(&repo, &["show", &format!("{published}:{path}")]).unwrap();
        assert!(
            !text.contains(&repo.display().to_string().replace('\\', "/")),
            "{text}"
        );
        assert!(!text.contains("verify-candidate"), "{text}");
        // A verifier on this clone agrees with the receipt (static checks).
        git(&repo, &["reset", "-q", "--hard", &published]).unwrap();
        let verification = gate_receipt::verify(
            &repo,
            &verified,
            &gate_receipt::Options {
                static_only: true,
                ..gate_receipt::Options::default()
            },
        )
        .unwrap();
        assert!(verification.all_match(), "{}", verification.render());
        // Without the recorded receipt commit, retry publishes nothing.
        git(
            &repo,
            &["update-ref", "-d", &gate_receipt::ref_for(&verified)],
        )
        .unwrap();
        remote.pushes.clear();
        let refused = ship_retry(&repo, &mut remote).unwrap_err();
        assert!(
            refused.to_string().contains("no gate receipt commit"),
            "{refused}"
        );
        assert!(remote.pushes.is_empty());
    }

    fn commit_on(repo: &Path, file: &str, content: &str) -> String {
        fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "--", file]).unwrap();
        git(repo, &["-c", "core.hooksPath=", "commit", "-qm", file]).unwrap();
        git(repo, &["rev-parse", "HEAD"]).unwrap()
    }

    fn bare_claim(repo: &Path, base: &str) -> Claim {
        Claim {
            name: "probe".into(),
            worktree: repo.to_path_buf(),
            base: base.into(),
            scopes: vec!["probe.rs".into()],
        }
    }

    #[test]
    fn scope_base_needs_no_receipt_when_the_claim_is_at_or_after_accepted() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let older = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let base = commit_on(&repo, "base.rs", "base");
        let source = commit_on(&repo, "probe.rs", "probe");
        // Integration ref equal to the claim base, with no receipt.
        git(&repo, &["update-ref", INTEGRATION_REF, &base]).unwrap();
        let claim = bare_claim(&repo, &base);
        assert_eq!(scope_base(&repo, &claim, &source).unwrap(), base);
        // Integration ref strictly behind the claim base, with no receipt.
        git(&repo, &["update-ref", INTEGRATION_REF, &older]).unwrap();
        assert_eq!(scope_base(&repo, &claim, &source).unwrap(), base);
    }

    #[test]
    fn ship_scope_base_falls_back_to_the_claim_base_without_a_receipt() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let accepted = commit_on(&repo, "accepted.rs", "accepted");
        let source = commit_on(&repo, "probe.rs", "probe");
        // Accepted is ahead of the claim base but carries no receipt.
        git(&repo, &["update-ref", INTEGRATION_REF, &accepted]).unwrap();
        let claim = bare_claim(&repo, &base);
        let error = scope_base(&repo, &claim, &source).unwrap_err();
        assert!(error.to_string().contains("no matching durable"), "{error}");
        // The fallback is the claim base, raised to what the source shares
        // with the publish base: `accepted` is already published, so only
        // `probe.rs` on top of it is this worker's change.
        assert_eq!(
            ship_scope_base(&repo, &claim, &source, &accepted).unwrap(),
            accepted
        );
        // With a publish base behind the claim base nothing is raised.
        assert_eq!(
            ship_scope_base(&repo, &claim, &source, &base).unwrap(),
            base
        );
        // A claim base that is not on the publish base still fails.
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
        assert!(ship_scope_base(&repo, &claim, &source, &unrelated).is_err());
    }

    #[test]
    fn ship_succeeds_and_chains_when_accepted_is_ahead_without_a_receipt() {
        let (_scratch, repo, _base) = ship_fixture("one");
        // Main advanced past the claim base and the integration ref points at
        // it, but this clone never wrote a receipt for that commit.
        let accepted = commit_on(&repo, "elsewhere.rs", "elsewhere");
        git(&repo, &["update-ref", INTEGRATION_REF, &accepted]).unwrap();
        let mut remote = FakeRemote::single(accepted.clone());
        let first = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(is_ancestor(&repo, &accepted, &first.head).unwrap());
        assert_eq!(remote.pushes.last().unwrap().0, first.head);
        // A later worker ships against the receipt the first ship wrote.
        worker(&repo, "two", "second.rs", "second");
        let mut remote = FakeRemote::single(first.head.clone());
        let second = ship(&repo, &["two"], &mut remote, |candidate, _attempt| {
            assert!(candidate.join("owned.rs").is_file());
            assert!(candidate.join("second.rs").is_file());
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(is_ancestor(&repo, &first.head, &second.head).unwrap());
    }

    #[test]
    fn workspace_proof_regate_retires_old_keys_for_partial_and_full_passes() {
        for full_regate in [false, true] {
            let (_scratch, repo, base) = ship_fixture("one");
            let moved = commit_on(&repo, "later.rs", "later");
            let mut remote = FakeRemote::single(base.clone());
            remote.mains = vec![base, moved.clone(), moved];
            let mut runs = 0;
            let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
                runs += 1;
                let mut evidence = local_gate_evidence();
                if runs == 1 || full_regate {
                    evidence.push(format!("{}proof-{runs}", workspace_test_proof::PREFIX));
                    evidence.push(format!("workspace-test-attestation-v1=attestation-{runs}"));
                }
                Ok(evidence)
            })
            .unwrap();
            assert_eq!(runs, 2);
            let tree = git(&repo, &["rev-parse", &format!("{}^{{tree}}", report.merge)]).unwrap();
            let receipt = matching_receipt(&repo, &report.merge, &tree)
                .unwrap()
                .unwrap();
            assert!(receipt.contains("historical workspace proof head="));
            let active: Vec<_> = receipt
                .lines()
                .filter(|line| {
                    line.starts_with(workspace_test_proof::PREFIX)
                        || line.starts_with("workspace-test-attestation-v1=")
                })
                .collect();
            if full_regate {
                assert_eq!(
                    active,
                    [
                        "workspace-test-pass-v1=proof-2",
                        "workspace-test-attestation-v1=attestation-2"
                    ]
                );
            } else {
                assert!(active.is_empty(), "{active:?}");
            }
        }
    }

    #[test]
    fn workspace_proof_failed_regate_never_publishes() {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved];
        let mut runs = 0;
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            runs += 1;
            if runs == 2 {
                return Err("failed re-gate".into());
            }
            let mut evidence = local_gate_evidence();
            evidence.push(format!("{}old", workspace_test_proof::PREFIX));
            Ok(evidence)
        })
        .unwrap_err();
        assert!(error.to_string().contains("failed re-gate"));
        assert!(remote.pushes.is_empty());
        assert!(integration_head(&repo).unwrap().is_none());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn workspace_proof_retirement_preserves_same_head_and_is_idempotent() {
        let original = vec![
            format!("{}key", workspace_test_proof::PREFIX),
            "cargo test => passed".into(),
        ];
        let mut evidence = original.clone();
        retire_workspace_proofs(&mut evidence, "same", "same");
        assert_eq!(evidence, original);
        retire_workspace_proofs(&mut evidence, "same", "new");
        let retired = evidence.clone();
        retire_workspace_proofs(&mut evidence, "new", "newer");
        assert_eq!(evidence, retired);
        assert_eq!(evidence[1], original[1]);
    }
    #[test]
    fn ship_reverifies_once_when_main_moves() {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved.clone(), moved];
        let runs = std::cell::Cell::new(0);
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            runs.set(runs.get() + 1);
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert_eq!(runs.get(), 2);
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(remote.pushes[0].0, report.head);
    }

    #[test]
    fn ship_regates_each_time_main_moves_and_publishes_on_the_latest() {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let third = commit_on(&repo, "third.rs", "third");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved, third.clone()];
        let runs = std::cell::Cell::new(0);
        let report = ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
            runs.set(runs.get() + 1);
            assert!(candidate.join("owned.rs").is_file());
            Ok(local_gate_evidence())
        })
        .unwrap();
        // No Cargo workspace in the fixture, so no selection: every re-gate runs.
        assert_eq!(runs.get(), 3);
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(remote.pushes[0].0, report.head);
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap(),
            third
        );
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{}^1", report.head)]).unwrap(),
            report.merge
        );
        let tree = git(&repo, &["ls-tree", "--name-only", &report.head]).unwrap();
        for file in ["owned.rs", "later.rs", "third.rs"] {
            assert!(tree.lines().any(|line| line == file), "{file}: {tree}");
        }
        let receipt_ref = git(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/integration/receipts/one/",
            ],
        )
        .unwrap();
        let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
        assert_eq!(receipt.matches("\nregate base=").count(), 2, "{receipt}");
        assert!(
            receipt.contains(&format!("regate base={third} ")),
            "{receipt}"
        );
        assert_eq!(integration_head(&repo).unwrap(), Some(report.merge));
    }

    #[test]
    fn ship_gives_up_without_publishing_when_main_moves_every_round() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut mains = vec![base];
        for round in 0..=SHIP_ROUNDS {
            mains.push(commit_on(
                &repo,
                &format!("later{round}.rs"),
                &format!("later {round}"),
            ));
        }
        let mut remote = FakeRemote::single(mains[0].clone());
        remote.mains = mains;
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("origin/main moved during each of"),
            "{error}"
        );
        assert!(remote.pushes.is_empty());
        assert!(integration_head(&repo).unwrap().is_none());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn ship_refuses_a_rewritten_main() {
        let (_scratch, repo, base) = ship_fixture("one");
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let unrelated = git(&repo, &["commit-tree", &tree, "-m", "rewritten"]).unwrap();
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, unrelated];
        let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap_err();
        assert!(error.to_string().contains("does not contain it"), "{error}");
        assert!(remote.pushes.is_empty());
    }

    /// One origin shared by concurrent ships: a push must fast-forward it.
    #[derive(Clone)]
    struct SharedRemote {
        main: std::sync::Arc<std::sync::Mutex<String>>,
        pushes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Remote for SharedRemote {
        fn fetch_main(&mut self, _root: &Path) -> Result<String> {
            Ok(self.main.lock().unwrap().clone())
        }
        fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()> {
            require_ship_ref(git_ref)?;
            let mut main = self.main.lock().unwrap();
            if !is_ancestor(root, &main, sha)? {
                return Err(format!("non-fast-forward push of {sha} over {main}").into());
            }
            sha.clone_into(&mut main);
            self.pushes.lock().unwrap().push(sha.to_owned());
            Ok(())
        }
    }

    /// The CEO's "no waiting" requirement: two ships' gates are inside
    /// verification at the same moment (each gate blocks until the other has
    /// started, so serialized gates fail here instead of passing slowly), in
    /// two different candidate worktrees, and both land on one origin/main by
    /// fast-forward with an intact receipt chain.
    #[test]
    fn concurrent_ships_gate_at_the_same_time_and_both_land() {
        use std::sync::{Arc, Condvar, Mutex};
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        worker(&repo, "one", "one.rs", "one");
        worker(&repo, "two", "two.rs", "two");
        let remote = SharedRemote {
            main: Arc::new(Mutex::new(base.clone())),
            pushes: Arc::new(Mutex::new(Vec::new())),
        };
        let inside = Arc::new((Mutex::new(0usize), Condvar::new()));
        let candidates = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for agent in ["one", "two"] {
            let repo = repo.clone();
            let mut remote = remote.clone();
            let inside = Arc::clone(&inside);
            let candidates = Arc::clone(&candidates);
            handles.push(std::thread::spawn(move || {
                ship(&repo, &[agent], &mut remote, |candidate, _attempt| {
                    candidates.lock().unwrap().push(candidate.to_path_buf());
                    let (count, entered) = &*inside;
                    let mut count = count.lock().unwrap();
                    *count += 1;
                    entered.notify_all();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                    while *count < 2 {
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        if left.is_zero() {
                            return Err("the other ship never gated concurrently".into());
                        }
                        count = entered.wait_timeout(count, left).unwrap().0;
                    }
                    Ok(local_gate_evidence())
                })
                .map(|report| report.head)
                .map_err(|error| error.to_string())
            }));
        }
        let heads: Vec<String> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();
        let pushes = remote.pushes.lock().unwrap().clone();
        assert_eq!(pushes.len(), 2, "{pushes:?}");
        let last = remote.main.lock().unwrap().clone();
        assert_eq!(&last, pushes.last().unwrap());
        assert!(heads.contains(&last));
        assert!(is_ancestor(&repo, &pushes[0], &pushes[1]).unwrap());
        let tree = git(&repo, &["ls-tree", "--name-only", &last]).unwrap();
        assert!(tree.lines().any(|line| line == "one.rs"), "{tree}");
        assert!(tree.lines().any(|line| line == "two.rs"), "{tree}");
        let candidates = candidates.lock().unwrap();
        let first_two: BTreeSet<_> = candidates.iter().take(2).collect();
        assert_eq!(
            first_two.len(),
            2,
            "both gates used one candidate: {candidates:?}"
        );
        // origin/main holds the last receipt commit; the integration ref and
        // the receipts name the verified merge under it.
        let verified_last = git(&repo, &["rev-parse", &format!("{last}^1")]).unwrap();
        assert_eq!(
            integration_head(&repo).unwrap(),
            Some(verified_last.clone())
        );
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        let tree_oid = git(&repo, &["rev-parse", &format!("{verified_last}^{{tree}}")]).unwrap();
        let receipt = matching_receipt(&repo, &verified_last, &tree_oid)
            .unwrap()
            .unwrap();
        assert!(receipt.contains("\nregate base="), "{receipt}");
    }

    fn cargo_workspace_fixture(repo: &Path) {
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"fixture-root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "").unwrap();
        for member in ["alpha", "beta"] {
            fs::create_dir_all(repo.join(member).join("src")).unwrap();
            fs::write(
                repo.join(member).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                ),
            )
            .unwrap();
            fs::write(repo.join(member).join("src/lib.rs"), "").unwrap();
        }
        let status = Command::new("cargo")
            .current_dir(repo)
            .args(["generate-lockfile", "--offline"])
            .status()
            .unwrap();
        assert!(status.success());
        git(
            repo,
            &["add", "Cargo.toml", "Cargo.lock", "src", "alpha", "beta"],
        )
        .unwrap();
        git(
            repo,
            &["-c", "core.hooksPath=", "commit", "-qm", "workspace"],
        )
        .unwrap();
    }

    #[test]
    fn regate_covers_only_what_the_newly_merged_commits_affect() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        cargo_workspace_fixture(&repo);
        let tested = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        fs::create_dir_all(repo.join("docs")).unwrap();
        commit_on(&repo, "docs/notes.md", "notes");
        // Docs: only the root package's tree-wide tests can observe them.
        let (needed, covers) = regate_needed(&repo, &tested);
        assert!(needed, "{covers}");
        assert!(
            covers.ends_with("select 1 packages: fixture-root"),
            "{covers}"
        );
        let docs = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        commit_on(&repo, "alpha/src/lib.rs", "pub fn alpha() -> u8 { 1 }\n");
        let (needed, covers) = regate_needed(&repo, &docs);
        assert!(needed, "{covers}");
        assert!(
            covers.ends_with("select 2 packages: alpha, fixture-root"),
            "{covers}"
        );
        // Nothing changed in the tree: the gates already run stand.
        let (needed, covers) = regate_needed(&repo, "HEAD");
        assert!(!needed, "{covers}");
    }

    #[test]
    fn ship_skips_the_regate_when_the_merged_tree_is_unchanged() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        cargo_workspace_fixture(&repo);
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        worker(&repo, "one", "alpha/src/one.rs", "pub fn one() {}\n");
        // origin/main gains a commit that changes no file.
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "empty",
            ],
        )
        .unwrap();
        let moved = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved.clone(), moved.clone()];
        let runs = std::cell::Cell::new(0);
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            runs.set(runs.get() + 1);
            let mut evidence = local_gate_evidence();
            evidence.push(format!("{}old", workspace_test_proof::PREFIX));
            evidence.push("workspace-test-attestation-v1=old".into());
            Ok(evidence)
        })
        .unwrap();
        assert_eq!(runs.get(), 1);
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap(),
            moved
        );
        let tree = git(&repo, &["rev-parse", &format!("{}^{{tree}}", report.merge)]).unwrap();
        let receipt = matching_receipt(&repo, &report.merge, &tree)
            .unwrap()
            .unwrap();
        assert!(!receipt
            .lines()
            .any(|line| line.starts_with(workspace_test_proof::PREFIX)
                || line.starts_with("workspace-test-attestation-v1=")));
        assert!(receipt.contains("historical workspace proof head="));
        assert!(
            receipt.contains(" skipped: the merged tree equals the tested tree"),
            "{receipt}"
        );
    }

    /// Regression (2026-11-06, commercial ships): the local integration ref
    /// was behind origin/main (main advanced through ships made elsewhere),
    /// and the receipt named that stale ref as `previous` although it is older
    /// than the claim base; every later ship failed at load. New receipts
    /// record the publish base; old ones verify by the published-lineage rule.
    #[test]
    fn a_stale_integration_ref_behind_main_keeps_receipts_verifiable() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        // A ship from this clone: the integration ref and its receipt.
        worker(&repo, "zero", "zero.rs", "zero");
        let mut remote = FakeRemote::single(base);
        let stale = ship(&repo, &["zero"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap()
        .merge;
        // origin/main then advanced through a ship made elsewhere, and the
        // next claim is reserved on top of it.
        git(
            &repo,
            &["-c", "core.hooksPath=", "merge", "--ff-only", "-q", &stale],
        )
        .unwrap();
        let advanced = commit_on(&repo, "elsewhere.rs", "elsewhere");
        assert_eq!(integration_head(&repo).unwrap(), Some(stale.clone()));
        worker(&repo, "one", "owned.rs", "owned");
        let mut remote = FakeRemote::single(advanced.clone());
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        let head = report.merge;
        let tree = git(&repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
        let receipt_ref = git(
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/susi/integration/receipts/one/",
            ],
        )
        .unwrap();
        let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
        assert!(
            receipt.contains(&format!("previous={advanced}")),
            "{receipt}"
        );
        assert!(receipt_matches(&repo, &receipt_ref, &receipt, &head, &tree).unwrap());
        let legacy = receipt.replace(
            &format!("previous={advanced}"),
            &format!("previous={stale}"),
        );
        assert!(receipt_matches(&repo, &receipt_ref, &legacy, &head, &tree).unwrap());
        // A later ship loads against that receipt.
        worker(&repo, "two", "second.rs", "second");
        let mut remote = FakeRemote::single(head.clone());
        let second = ship(&repo, &["two"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert!(is_ancestor(&repo, &head, &second.head).unwrap());
    }

    #[test]
    fn gate_share_divides_the_host_instead_of_queueing() {
        assert_eq!(
            gate_share(28, 1),
            GateShare {
                live: 1,
                jobs: 28,
                test_threads: 28,
                nextest_threads: 16
            }
        );
        assert_eq!(
            gate_share(28, 2),
            GateShare {
                live: 2,
                jobs: 14,
                test_threads: 14,
                nextest_threads: 8
            }
        );
        assert_eq!(
            gate_share(28, 3),
            GateShare {
                live: 3,
                jobs: 9,
                test_threads: 9,
                nextest_threads: 5
            }
        );
        assert_eq!(gate_share(28, 40).jobs, 2);
        assert_eq!(gate_share(28, 40).nextest_threads, 2);
        assert_eq!(gate_share(28, 0), gate_share(28, 1));
    }

    #[test]
    fn commercial_process_heavy_share_caps_only_windows_test_fanout() {
        let base = gate_share(28, 1);
        for args in [
            vec!["test", "--workspace", "--locked"],
            vec!["test", "-p", "susi-dev-workflow", "--locked"],
            vec!["test", "--package", "susi-ecosystem-hub"],
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            let bounded = commercial_command_share(base, &args, true);
            assert_eq!(bounded.test_threads, 8);
            assert_eq!(bounded.jobs, base.jobs);
            assert_eq!(bounded.nextest_threads, base.nextest_threads);
            assert_eq!(bounded.live, base.live);
            assert_eq!(commercial_command_share(base, &args, false), base);
            let smaller = gate_share(4, 1);
            assert_eq!(commercial_command_share(smaller, &args, true), smaller);
            let mut command = Command::new("cargo");
            apply_gate_share(&mut command, bounded);
            for (key, expected) in [("CARGO_BUILD_JOBS", "28"), ("RUST_TEST_THREADS", "8")] {
                assert!(command.get_envs().any(|(name, value)| {
                    name == key && value == Some(std::ffi::OsStr::new(expected))
                }));
            }
        }
        for args in [
            vec!["clippy", "--workspace"],
            vec!["test", "-p", "susi-reflex-modules"],
            vec!["test", "--package"],
            vec![],
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            assert_eq!(commercial_command_share(base, &args, true), base);
        }
    }

    #[test]
    fn integration_gate_disables_registry_environment() {
        let mut command = Command::new("unused");
        apply_gate_env(
            &mut command,
            &GateEnv {
                home: Path::new("private/home"),
                cargo_home: Path::new("toolchain/cargo"),
                rustup_home: Path::new("toolchain/rustup"),
                target: Path::new("private/target"),
                sysroot: None,
            },
        )
        .unwrap();
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
                && value == Some(std::ffi::OsStr::new("1"))));
    }
    /// Regression (2026-11-06): `check` isolated HOME/USERPROFILE but not the
    /// toolchain homes, so tests that run `cargo tree --offline` found an
    /// empty cargo home and failed only under `check`.
    #[test]
    fn check_commands_keep_the_real_toolchain_homes_under_an_isolated_home() {
        let top = Path::new("worktree");
        let home = Path::new("worktree/target/susi-check-home");
        let command = check_command(
            top,
            &["test".to_owned()],
            Path::new("worktree/target"),
            home,
            (Path::new("real/.cargo"), Path::new("real/.rustup")),
        );
        let envs: std::collections::BTreeMap<String, Option<String>> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let get = |key: &str| envs.get(key).cloned().flatten();
        assert_eq!(get("SUSI_NO_REGISTRY_ENV").as_deref(), Some("1"));
        assert_eq!(get("CARGO_HOME").as_deref(), Some("real/.cargo"));
        assert_eq!(get("RUSTUP_HOME").as_deref(), Some("real/.rustup"));
        assert_eq!(get("CARGO_TARGET_DIR").as_deref(), Some("worktree/target"));
        for key in ["HOME", "USERPROFILE"] {
            assert_eq!(
                get(key).as_deref(),
                Some("worktree/target/susi-check-home"),
                "{key}"
            );
        }
    }

    /// A process that stays alive until killed: `git cat-file --batch` waits
    /// on its open stdin.
    fn live_child() -> std::process::Child {
        Command::new("git")
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn locks_mark_their_owner_as_a_slots_launcher() {
        let scratch = Scratch::new();
        let state = scratch.0.join("state");
        let lock = Lock::acquire(&state).unwrap();
        let owner = fs::read_to_string(state.join("lock").join("owner")).unwrap();
        assert!(owner.lines().any(|line| line == SLOTS_LAUNCHER), "{owner}");
        drop(lock);
    }

    #[test]
    fn legacy_owner_metadata_rejects_malformed_and_self_identity() {
        let scratch = Scratch::new();
        let dir = scratch.0.join("owner-probe");
        fs::create_dir_all(&dir).unwrap();
        assert!(!legacy_owner_alive(&dir));
        for record in [
            "pid=not-a-number\n".to_owned(),
            format!("pid={}\n", std::process::id()),
        ] {
            fs::write(dir.join("owner"), record).unwrap();
            assert!(!legacy_owner_alive(&dir));
        }
    }
    /// Transition (2026-11-07): a launcher from before per-ship slots holds
    /// `integration` for its whole gate and its `integrate` moves the accepted
    /// ref without `publish`; a slot ship's publication waits for it, but
    /// never for a slots launcher's `integration` holder.
    #[test]
    fn legacy_integration_owner_is_only_a_live_pre_slots_launcher() {
        let scratch = Scratch::new();
        let workflow = scratch.0.join("workflow");
        let dir = workflow.join("integration").join("lock");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(legacy_integration_owner(&workflow), None);
        let mut child = live_child();
        let pid = child.id();
        fs::write(dir.join("owner"), format!("pid={pid}\n")).unwrap();
        assert_eq!(legacy_integration_owner(&workflow), Some(pid));
        fs::write(dir.join("owner"), format!("pid={pid}\n{SLOTS_LAUNCHER}\n")).unwrap();
        assert_eq!(legacy_integration_owner(&workflow), None);
        fs::write(dir.join("owner"), format!("pid={pid}\n")).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(legacy_integration_owner(&workflow), None);
    }

    #[test]
    fn ship_publishes_only_after_a_legacy_integration_holder_exits() {
        use std::sync::{Arc, Mutex};
        let (_scratch, repo, base) = ship_fixture("one");
        let dir = state(&repo).unwrap().join("integration").join("lock");
        fs::create_dir_all(&dir).unwrap();
        let mut child = live_child();
        fs::write(dir.join("owner"), format!("pid={}\n", child.id())).unwrap();
        let remote = SharedRemote {
            main: Arc::new(Mutex::new(base)),
            pushes: Arc::new(Mutex::new(Vec::new())),
        };
        let mut shipping = remote.clone();
        let (gated, gate_done) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            ship(&repo, &["one"], &mut shipping, |_candidate, _attempt| {
                let _ = gated.send(());
                Ok(local_gate_evidence())
            })
            .map(|report| report.head)
            .map_err(|error| error.to_string())
        });
        gate_done
            .recv_timeout(std::time::Duration::from_secs(120))
            .unwrap();
        // The gate has passed; publication must still be waiting.
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(remote.pushes.lock().unwrap().is_empty());
        child.kill().unwrap();
        child.wait().unwrap();
        let head = handle.join().unwrap().unwrap();
        assert_eq!(remote.pushes.lock().unwrap().as_slice(), [head]);
    }

    #[test]
    fn live_gates_count_running_markers_and_drop_dead_ones() {
        let scratch = Scratch::new();
        let dir = scratch.0.join("live-gates");
        assert_eq!(live_gates(&dir), 1);
        let first = register_live_gate(&dir).unwrap();
        let second = register_live_gate(&dir).unwrap();
        assert_eq!(live_gates(&dir), 2);
        // A crashed gate leaves an unlocked marker behind.
        let dead = dir.join("4242-1-0.gate");
        fs::write(&dead, "").unwrap();
        assert_eq!(live_gates(&dir), 2);
        assert!(!dead.exists());
        drop(first);
        assert_eq!(live_gates(&dir), 1);
        drop(second);
        assert_eq!(live_gates(&dir), 1);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn slots_are_private_never_waited_for_and_reused() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let first = acquire_slot(&repo).unwrap();
        let second = acquire_slot(&repo).unwrap();
        assert_ne!(first.candidate, second.candidate);
        assert_ne!(first.attempt, second.attempt);
        for slot in [&first, &second] {
            let name = slot.candidate.file_name().unwrap().to_str().unwrap();
            assert!(name.starts_with("repo-verify-candidate-"), "{name}");
        }
        let freed = first.candidate.clone();
        drop(first);
        assert_eq!(acquire_slot(&repo).unwrap().candidate, freed);
        let workflow = state(&repo).unwrap();
        assert_eq!(
            warm_targets(&workflow, &second.candidate),
            vec![
                workflow.join("slots").join("2").join("target"),
                workflow.join("integration-target"),
            ]
        );
        assert_eq!(
            warm_targets(&workflow, &candidate_path(&repo).unwrap()),
            vec![workflow.join("integration-target")]
        );
    }

    /// Regression (2026-11-06, dsp4c1-1106): a leftover candidate that could
    /// not be removed made the next ship fail at `worktree add` with a
    /// misleading error, after the removal error had been ignored. A slot
    /// whose leftover cannot be cleared is now skipped (no waiting), and the
    /// removal error itself is what a direct merge reports.
    #[test]
    fn a_slot_whose_leftover_candidate_cannot_be_removed_is_skipped() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let mut blocked = candidate_path(&repo).unwrap().into_os_string();
        blocked.push("-1");
        let blocked = PathBuf::from(blocked);
        // A path that directory removal cannot clear stands in for a target
        // whose files another process still holds open.
        fs::write(&blocked, "held").unwrap();
        let error = clear_candidate(&repo, &blocked).unwrap_err().to_string();
        assert!(
            error.contains("cannot remove the previous candidate"),
            "{error}"
        );
        let slot = acquire_slot(&repo).unwrap();
        assert!(
            slot.candidate
                .to_string_lossy()
                .ends_with("-verify-candidate-2"),
            "{}",
            slot.candidate.display()
        );
        assert!(blocked.is_file());
    }

    #[test]
    fn a_slot_adopts_the_legacy_warm_target_once_and_keeps_its_own() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let workflow = state(&repo).unwrap();
        let legacy = workflow.join("integration-target");
        fs::create_dir_all(legacy.join("debug")).unwrap();
        let slot = acquire_slot(&repo).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                text(&slot.candidate).unwrap(),
                "HEAD",
            ],
        )
        .unwrap();
        let target = restore_candidate_target(&slot.candidate);
        assert!(target.join("debug").is_dir());
        assert!(!legacy.exists());
        stash_candidate_target(&repo, &slot.candidate);
        assert!(workflow.join("slots/1/target/debug").is_dir());
        assert!(!legacy.exists());
    }

    #[test]
    fn ship_retry_requires_a_matching_receipt() {
        let (_scratch, repo, base) = ship_fixture("one");
        let mut remote = FakeRemote::single(base.clone());
        let missing = ship_retry(&repo, &mut remote).unwrap_err();
        assert!(missing.to_string().contains("missing receipt"), "{missing}");
        assert!(remote.pushes.is_empty());
        remote.fail_push = true;
        let pushed = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            Ok(local_gate_evidence())
        })
        .unwrap_err();
        assert!(pushed.to_string().contains("push refused"), "{pushed}");
        remote.fail_push = false;
        remote.pushes.clear();
        let report = ship_retry(&repo, &mut remote).unwrap();
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(remote.pushes[0].0, report.head);
        assert_eq!(remote.pushes[0].1, "refs/heads/main");
    }

    #[test]
    fn susi_checkout_is_susi_kind() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask parent");
        assert_eq!(repo_kind(root).unwrap(), RepoKind::Susi);
    }

    #[test]
    fn commercial_package_is_commercial_kind() {
        let scratch = Scratch::new();
        fs::write(
            scratch.0.join("Cargo.toml"),
            "[package]\nname = \"susi-commercial\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        assert_eq!(repo_kind(&scratch.0).unwrap(), RepoKind::Commercial);
    }

    #[test]
    fn unrecognized_checkout_is_refused() {
        let scratch = Scratch::new();
        let error = repo_kind(&scratch.0).unwrap_err();
        assert!(
            error.to_string().contains("unrecognized checkout"),
            "{error}"
        );
    }

    #[test]
    fn commercial_gate_plan_does_not_call_susi_proofs() {
        let plan = commercial_gate_argv();
        let flat = plan
            .iter()
            .flat_map(|args| args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        for needle in ["fmt", "clippy", "test", "deny"] {
            assert!(flat.contains(needle), "{flat}");
        }
        assert!(!flat.contains("trusted_sysroot"), "{flat}");
        assert!(!flat.contains("http://"), "{flat}");
        assert!(!flat.contains("https://"), "{flat}");
    }

    #[test]
    fn usage_lists_ship_commands() {
        let error = run(&["nope".into()]).unwrap_err();
        let text = error.to_string();
        for needle in ["ship <name>", "ship --retry", "status [--json]"] {
            assert!(text.contains(needle), "{text}");
        }
        // GitHub is storage only: no command administers the host.
        for retired in ["ruleset", "workflows"] {
            assert!(!text.contains(retired), "{text}");
        }
        assert!(run(&["status".into(), "--yaml".into()]).is_err());
    }

    #[test]
    fn start_reuses_a_clean_released_worktree_and_keeps_its_target() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let scopes = vec!["owned.rs".to_owned()];
        let worktree = start(&repo, "again", &scopes).unwrap();
        let sentinel = worktree.join("target").join("sentinel.txt");
        fs::write(&sentinel, "warm").unwrap();
        release(&repo, "again").unwrap();
        // main moves on while the worktree sits released.
        let moved = commit_on(&repo, "later.rs", "later");

        let reused = start(&repo, "again", &scopes).unwrap();
        assert_eq!(reused, worktree);
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "warm");
        assert_eq!(
            git(&reused, &["branch", "--show-current"]).unwrap(),
            "local-agent/again"
        );
        assert_eq!(git(&reused, &["rev-parse", "HEAD"]).unwrap(), moved);
        let claim = read_claim(&state(&repo).unwrap().join("claims").join("again.claim")).unwrap();
        assert_eq!(claim.base, moved);
        assert_eq!(claim.scopes, scopes);

        // Dirty: an untracked file blocks reuse and leaves no claim behind.
        release(&repo, "again").unwrap();
        fs::write(reused.join("stray.rs"), "stray").unwrap();
        let error = start(&repo, "again", &scopes).unwrap_err();
        assert!(error.to_string().contains("refusing to reuse"), "{error}");
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        fs::remove_file(reused.join("stray.rs")).unwrap();

        // Committed but unshipped work on the agent branch is never reset.
        fs::write(reused.join("owned.rs"), "unshipped").unwrap();
        git(&reused, &["add", "owned.rs"]).unwrap();
        git(
            &reused,
            &["-c", "core.hooksPath=", "commit", "-qm", "unshipped"],
        )
        .unwrap();
        let error = start(&repo, "again", &scopes).unwrap_err();
        assert!(error.to_string().contains("holds commits"), "{error}");
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
        assert!(sentinel.is_file());
    }

    #[test]
    fn start_refuses_an_unregistered_destination() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let ghost = repo.parent().unwrap().join("repo-agent-ghost");
        fs::create_dir(&ghost).unwrap();
        let error = start(&repo, "ghost", &["owned.rs".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("refusing to reuse"), "{error}");
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn status_reports_claims_slots_and_integration_without_the_lock() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let empty = workflow_status(&repo).unwrap();
        assert!(empty.claims.is_empty() && empty.busy_slots.is_empty());
        assert_eq!(empty.integration_head, None);
        assert!(status_text(&empty).contains("integration: none"));

        start(&repo, "alpha", &["alpha.rs".to_owned()]).unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        git(&repo, &["update-ref", INTEGRATION_REF, &head]).unwrap();
        let slot = acquire_slot(&repo).unwrap();
        // Holding the workflow lock proves status never asks for it: a
        // second acquire on this thread fails instead of waiting.
        let _lock = Lock::acquire(&state(&repo).unwrap()).unwrap();
        let status = workflow_status(&repo).unwrap();
        assert_eq!(status.integration_head.as_deref(), Some(head.as_str()));
        assert_eq!(status.claims.len(), 1);
        assert_eq!(status.busy_slots.len(), 1);
        assert_eq!(status.busy_slots[0].slot, 1);
        assert!(status.busy_slots[0].owner.starts_with("pid="));

        let text = status_text(&status);
        for needle in [
            format!("integration: {head}"),
            "claims: 1".to_owned(),
            "alpha".to_owned(),
            "alpha.rs".to_owned(),
            "verifying: 1".to_owned(),
            "slot 1".to_owned(),
        ] {
            assert!(text.contains(&needle), "{text}");
        }

        let json = status_json(&status);
        assert_eq!(json["schema"], "susi-local-workflow-status-v1");
        assert_eq!(json["integration_head"], head.as_str());
        assert_eq!(json["claims"][0]["name"], "alpha");
        assert_eq!(json["claims"][0]["scopes"][0], "alpha.rs");
        assert_eq!(json["busy_slots"][0]["slot"], 1);

        drop(slot);
        assert!(workflow_status(&repo).unwrap().busy_slots.is_empty());
    }

    #[test]
    fn ten_parallel_agents_start_commit_extend_and_ship_without_conflict() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(10));
        let mut handles = Vec::new();
        for index in 0..10 {
            let repo = repo.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let agent = format!("agent{index:02}");
                let scope_a = format!("crates/mod{index:02}/a.rs");
                let scope_b = format!("crates/mod{index:02}/b.rs");
                let scope_c = format!("crates/mod{index:02}/c.rs");
                barrier.wait();
                let worktree = start(&repo, &agent, &[scope_a.clone(), scope_c.clone()]).unwrap();
                extend(&worktree, &agent, std::slice::from_ref(&scope_b)).unwrap();
                update_scopes(
                    &worktree,
                    &agent,
                    std::slice::from_ref(&scope_c),
                    ScopeChange::Shrink,
                )
                .unwrap();
                fs::create_dir_all(worktree.join(format!("crates/mod{index:02}"))).unwrap();
                fs::write(worktree.join(&scope_a), format!("pub fn a{index}() {{}}\n")).unwrap();
                fs::write(worktree.join(&scope_b), format!("pub fn b{index}() {{}}\n")).unwrap();
                commit(
                    &worktree,
                    &agent,
                    &format!("feat(mod{index:02}): parallel agent {index}\n\nTask: LOCAL-{agent}"),
                    None,
                )
                .unwrap();
                agent
            }));
        }
        let mut agents = Vec::new();
        for handle in handles {
            agents.push(handle.join().unwrap());
        }
        agents.sort();
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 10);
        let agent_refs: Vec<&str> = agents.iter().map(String::as_str).collect();
        let mut remote = FakeRemote::single(base);
        let report = ship(&repo, &agent_refs, &mut remote, |candidate, _attempt| {
            for index in 0..10 {
                assert!(candidate
                    .join(format!("crates/mod{index:02}/a.rs"))
                    .is_file());
                assert!(candidate
                    .join(format!("crates/mod{index:02}/b.rs"))
                    .is_file());
            }
            Ok(local_gate_evidence())
        })
        .unwrap();
        assert_eq!(remote.pushes.len(), 1);
        assert_eq!(remote.pushes[0].0, report.head);
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn every_workflow_step_pulls_and_reconciles_bare_origin_with_offline_fallback() {
        let scratch = Scratch::new();
        let repo = scratch.repo();
        let origin_bare = repo.parent().unwrap().join("origin-bare.git");
        git(
            &repo,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                text(&origin_bare).unwrap(),
            ],
        )
        .unwrap();
        git(
            &repo,
            &["remote", "add", "origin", text(&origin_bare).unwrap()],
        )
        .unwrap();
        let initial_base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                "origin",
                &format!("{initial_base}:refs/heads/main"),
            ],
        )
        .unwrap();

        // Create a peer clone simulating concurrent remote contributors on `origin/main`.
        let peer = repo.parent().unwrap().join("peer-clone");
        git(
            &repo,
            &[
                "clone",
                "--quiet",
                text(&origin_bare).unwrap(),
                text(&peer).unwrap(),
            ],
        )
        .unwrap();
        git(&peer, &["config", "user.name", "Peer"]).unwrap();
        git(&peer, &["config", "user.email", "peer@example.invalid"]).unwrap();

        let peer_push = |rel: &str, body: &str, task_id: &str| -> String {
            let full = peer.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, body).unwrap();
            let task_rel = format!(".agents/tasks/{task_id}.json");
            fs::create_dir_all(peer.join(".agents/tasks")).unwrap();
            let task_doc = serde_json::json!({
                "id": task_id,
                "title": format!("Task {task_id}"),
                "goal": format!("Goal {task_id}"),
                "created_by": "peer",
                "created_unix": 1_794_300_000u64,
                "status": "done",
                "deps": [],
                "scopes": [rel, task_rel],
                "accept": { "cmd": ["cargo", "test", "--offline", "--locked"] }
            });
            fs::write(
                peer.join(&task_rel),
                format!("{}\n", serde_json::to_string_pretty(&task_doc).unwrap()),
            )
            .unwrap();
            git(&peer, &["add", "-A"]).unwrap();
            git(
                &peer,
                &[
                    "-c",
                    "core.hooksPath=",
                    "commit",
                    "-qm",
                    &format!("feat: {task_id}\n\nTask: {task_id}"),
                ],
            )
            .unwrap();
            git(
                &peer,
                &[
                    "-c",
                    "core.hooksPath=",
                    "push",
                    "--quiet",
                    "origin",
                    "HEAD:refs/heads/main",
                ],
            )
            .unwrap();
            git(&peer, &["rev-parse", "HEAD"]).unwrap()
        };

        // 1. `start` pulls peer commit P1 from `origin/main` before creating worktree.
        let p1 = peer_push("peer/step1.rs", "pub fn step1() {}\n", "T-STEP-1");
        let wt_alpha = start(&repo, "alpha", &["alpha/mod.rs".to_owned()]).unwrap();
        assert_eq!(git(&wt_alpha, &["rev-parse", "HEAD"]).unwrap(), p1);
        assert!(wt_alpha.join("peer/step1.rs").is_file());

        // 2. `sync` (`step`) fast-forwards `local-agent/alpha` when peer pushes P2 before alpha commits.
        let p2 = peer_push("peer/step2.rs", "pub fn step2() {}\n", "T-STEP-2");
        let ff_report = sync_claim(&wt_alpha, "alpha").unwrap();
        assert_eq!(ff_report.mode, AgentSyncMode::FastForwarded);
        assert_eq!(ff_report.head, p2);
        assert!(wt_alpha.join("peer/step2.rs").is_file());

        // 3. `commit` creates alpha's scoped commit AND auto-pulls + 3-way merges peer commit P3!
        let p3 = peer_push("peer/step3.rs", "pub fn step3() {}\n", "T-STEP-3");
        fs::create_dir_all(wt_alpha.join("alpha")).unwrap();
        fs::write(
            wt_alpha.join("alpha/mod.rs"),
            "pub fn alpha() -> u32 { 7 }\n",
        )
        .unwrap();
        commit(&wt_alpha, "alpha", "feat: alpha module", None).unwrap();
        let after_commit = git(&wt_alpha, &["rev-parse", "HEAD"]).unwrap();
        assert!(is_ancestor(&wt_alpha, &p3, &after_commit).unwrap());
        assert!(wt_alpha.join("peer/step3.rs").is_file());
        assert!(wt_alpha.join("alpha/mod.rs").is_file());
        // Scope guard still sees ONLY alpha's scoped file relative to updated claim.base!
        let wf = state(&repo).unwrap();
        let alpha_claim = read_local_claim(&wf.join("claims/alpha.claim")).unwrap();
        assert_eq!(alpha_claim.base, p3);
        check_source(&repo, &alpha_claim).unwrap();

        // 4. Offline fallback: point origin at a disconnected path and verify `sync` succeeds cleanly.
        let disconnected = repo.parent().unwrap().join("disconnected-origin.git");
        git(
            &repo,
            &["remote", "set-url", "origin", text(&disconnected).unwrap()],
        )
        .unwrap();
        let offline_sync = sync_claim(&wt_alpha, "alpha").unwrap();
        assert_eq!(offline_sync.mode, AgentSyncMode::OfflineFallback);
        git(
            &repo,
            &["remote", "set-url", "origin", text(&origin_bare).unwrap()],
        )
        .unwrap();

        // 5. `ship` reconciles with another peer commit P4 on real `GitRemote` and fast-forwards `origin/main`.
        let p4 = peer_push("peer/step4.rs", "pub fn step4() {}\n", "T-STEP-4");
        let mut real_remote = GitRemote;
        let shipped = ship(
            &repo,
            &["alpha"],
            &mut real_remote,
            |candidate, _attempt| {
                assert!(candidate.join("alpha/mod.rs").is_file());
                assert!(candidate.join("peer/step4.rs").is_file());
                Ok(local_gate_evidence())
            },
        )
        .unwrap();
        assert!(is_ancestor(&repo, &p4, &shipped.head).unwrap());

        // 6. `release`: start beta, land beta's commit onto `origin/main` via peer, and verify `release` pulls `origin/main`!
        let wt_beta = start(&repo, "beta", &["beta/mod.rs".to_owned()]).unwrap();
        fs::create_dir_all(wt_beta.join("beta")).unwrap();
        fs::write(wt_beta.join("beta/mod.rs"), "pub fn beta() {}\n").unwrap();
        commit_with_pull(&wt_beta, "beta", "feat: beta module", None, false).unwrap();
        let beta_head = git(&wt_beta, &["rev-parse", "HEAD"]).unwrap();
        // Push beta_head directly to bare origin WITHOUT updating local refs/remotes/origin/main first.
        git(
            &wt_beta,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                text(&origin_bare).unwrap(),
                &format!("{beta_head}:refs/heads/main"),
            ],
        )
        .unwrap();
        release(&repo, "beta").unwrap();
        assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());

        // 7. `workflow_distill`: pulls `origin/main` and distills T-STEP-1..T-STEP-4 into a release snapshot + record!
        let record_out = repo.parent().unwrap().join("v0.21.61.json");
        let distill_out = workflow_distill(
            &repo,
            &[
                "compose".into(),
                "--name".into(),
                "wf-rc1".into(),
                "--base".into(),
                initial_base,
                "--from".into(),
                "origin/main".into(),
                "--select".into(),
                "T-STEP-1,T-STEP-2,T-STEP-3,T-STEP-4".into(),
                "--release-candidate".into(),
                "--strict".into(),
                "--release-tag".into(),
                "v0.21.61".into(),
                "--release-record-out".into(),
                text(&record_out).unwrap().to_owned(),
                "--json".into(),
            ],
        )
        .unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&distill_out).unwrap();
        assert_eq!(receipt["included"].as_array().unwrap().len(), 4);
        assert!(record_out.is_file());
    }
}
