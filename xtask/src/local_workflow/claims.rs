use super::*;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(in crate::local_workflow) fn name(value: &str) -> Result<()> {
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

pub(in crate::local_workflow) fn reserved(value: &str) -> bool {
    common::is_reserved_windows_device(value)
}

pub(in crate::local_workflow) fn scope(value: &str) -> Result<String> {
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

pub(in crate::local_workflow) fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}

thread_local! {
    static HELD_LOCKS: std::cell::RefCell<BTreeSet<PathBuf>> =
        const { std::cell::RefCell::new(BTreeSet::new()) };
}

pub(in crate::local_workflow) struct Lock {
    pub(in crate::local_workflow) dir: PathBuf,
    pub(in crate::local_workflow) key: PathBuf,
    pub(in crate::local_workflow) _file: fs::File,
}
impl Lock {
    pub(in crate::local_workflow) fn acquire(state: &Path) -> Result<Self> {
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
pub(in crate::local_workflow) const SLOTS_LAUNCHER: &str = "launcher=slots";

/// The pid of a live launcher from before per-ship slots that holds
/// `integration`. Such a launcher holds it for a whole `ship` or `integrate`
/// gate, and its `integrate` moves `refs/susi/integration/current` without
/// taking `publish`; a slot ship's publication waits for it rather than race
/// it during the transition. A launcher with slots is never waited for here:
/// its `integrate` takes `publish` for its ref transaction.
pub(in crate::local_workflow) fn legacy_integration_owner(workflow: &Path) -> Option<u32> {
    let dir = workflow.join("integration").join("lock");
    let text = fs::read_to_string(dir.join("owner")).ok()?;
    if text.lines().any(|line| line.trim() == SLOTS_LAUNCHER) || !legacy_owner_alive(&dir) {
        return None;
    }
    text.lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.trim().parse().ok())
}

pub(in crate::local_workflow) fn wait_for_legacy_integration(workflow: &Path) {
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

pub(in crate::local_workflow) fn legacy_owner_alive(dir: &Path) -> bool {
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
pub(in crate::local_workflow) struct HostGate {
    pub(in crate::local_workflow) _file: fs::File,
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
pub(in crate::local_workflow) fn release_probe(file: fs::File) {
    let _ = file.unlock();
}

/// Per-user state shared by every repository on this host:
/// `%LOCALAPPDATA%\susi-local-workflow` on Windows,
/// `~/.cache/susi-local-workflow` elsewhere.
pub(in crate::local_workflow) fn host_state_dir() -> Result<PathBuf> {
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
pub(in crate::local_workflow) fn acquire_host_gate(dir: &Path, holder: &str) -> Result<HostGate> {
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
pub(in crate::local_workflow) struct LiveGate {
    pub(in crate::local_workflow) file: fs::File,
    pub(in crate::local_workflow) path: PathBuf,
}

impl Drop for LiveGate {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

pub(in crate::local_workflow) fn live_gate_dir() -> Result<PathBuf> {
    Ok(host_state_dir()?.join("live-gates"))
}

/// Register a running gate in `dir`. The marker is locked under a temporary
/// name and only then renamed to `*.gate`, so a concurrent count never sees
/// (and never removes as stale) a marker that is not locked yet.
pub(in crate::local_workflow) fn register_live_gate(dir: &Path) -> Result<LiveGate> {
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

pub(in crate::local_workflow) static LIVE_GATE_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// How many gates are running on this host now, the caller's included (at
/// least 1). A marker whose lock can be taken belongs to a gate that exited or
/// crashed; it is removed. Entries that cannot be opened are not counted.
pub(in crate::local_workflow) fn live_gates(dir: &Path) -> usize {
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
pub(in crate::local_workflow) struct GateShare {
    pub(in crate::local_workflow) live: usize,
    /// `CARGO_BUILD_JOBS`.
    pub(in crate::local_workflow) jobs: usize,
    /// `RUST_TEST_THREADS` for `cargo test`.
    pub(in crate::local_workflow) test_threads: usize,
    /// `NEXTEST_TEST_THREADS` for `cargo nextest run`.
    pub(in crate::local_workflow) nextest_threads: usize,
}

/// Divide `cores` logical CPUs evenly between `live` gates, never below 2
/// jobs or test threads. A single gate keeps the settings it had when gates
/// queued: every core for cargo and `cargo test`, `GATE_TEST_THREADS` for
/// nextest. Lowering each gate's parallelism replaces the old host-wide
/// queue: the wall-clock deadline tests failed (2026-11-06) when two gates
/// each ran on every core, which an even share avoids.
pub(in crate::local_workflow) fn gate_share(cores: usize, live: usize) -> GateShare {
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
pub(in crate::local_workflow) fn current_gate_share() -> GateShare {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let live = live_gate_dir().map_or(1, |dir| live_gates(&dir));
    gate_share(cores, live)
}

pub(in crate::local_workflow) fn apply_gate_share(command: &mut Command, share: GateShare) {
    command
        .env("CARGO_BUILD_JOBS", share.jobs.to_string())
        .env("RUST_TEST_THREADS", share.test_threads.to_string())
        .env("NEXTEST_TEST_THREADS", share.nextest_threads.to_string());
}

/// Windows process-heavy commercial tests share launcher/handle resources in
/// addition to CPUs. Bound their libtest fan-out without reducing compiler
/// parallelism, test selection, or any deadline. Other gates keep their share.
pub(in crate::local_workflow) fn commercial_command_share(
    mut share: GateShare,
    args: &[String],
    windows: bool,
) -> GateShare {
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

pub(in crate::local_workflow) fn print_gate_share(command: &str, share: GateShare) {
    eprintln!(
        "gate share for cargo {command}: {} live gate(s) on this host; {} build jobs, {} test threads",
        share.live, share.jobs, share.test_threads
    );
}

/// Register this process's gate on the host, or carry on unregistered (and
/// so uncounted) when the per-user state directory is unusable. The count is
/// a CPU-sharing input, never a correctness condition.
pub(in crate::local_workflow) fn live_gate() -> Option<LiveGate> {
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
pub(in crate::local_workflow) struct Slot {
    pub(in crate::local_workflow) candidate: PathBuf,
    pub(in crate::local_workflow) attempt: PathBuf,
    pub(in crate::local_workflow) _lock: HostGate,
}

pub(in crate::local_workflow) const MAX_SLOTS: usize = 64;

pub(in crate::local_workflow) fn acquire_slot(root: &Path) -> Result<Slot> {
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
pub(in crate::local_workflow) fn include_release_tooling_for_xtask(
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
pub(in crate::local_workflow) struct Claim {
    pub(in crate::local_workflow) name: String,
    pub(in crate::local_workflow) worktree: PathBuf,
    pub(in crate::local_workflow) base: String,
    pub(in crate::local_workflow) scopes: Vec<String>,
}

pub(in crate::local_workflow) fn read_claim(path: &Path) -> Result<Claim> {
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
pub(in crate::local_workflow) fn absolute_on_some_host(recorded: &str) -> bool {
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
    pub(in crate::local_workflow) fn is_local(&self) -> bool {
        self.worktree.is_absolute()
    }

    pub(in crate::local_workflow) fn host_label(&self) -> &'static str {
        if self.is_local() {
            ""
        } else {
            " [other host]"
        }
    }
}

/// Read a claim whose worktree must be operated on from this host.
pub(in crate::local_workflow) fn read_local_claim(path: &Path) -> Result<Claim> {
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

pub(in crate::local_workflow) fn claims(state: &Path) -> Result<Vec<Claim>> {
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

pub(in crate::local_workflow) fn state(root: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .join("susi-local-workflow"))
}

pub(in crate::local_workflow) const INTEGRATION_REF: &str = "refs/susi/integration/current";

/// Wall-clock phases of the current `ship`/`integrate`, in order.
pub(in crate::local_workflow) static PHASES: std::sync::Mutex<Vec<(String, u128)>> =
    std::sync::Mutex::new(Vec::new());

/// Run `work` as the named phase: print its wall-clock time on stderr and
/// remember it for the run summary, so every gate run shows where its minutes
/// went instead of leaving setup and publication unmeasured.
pub(in crate::local_workflow) fn timed<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let value = work();
    record_phase(name, started.elapsed().as_millis());
    value
}

pub(in crate::local_workflow) fn record_phase(name: &str, millis: u128) {
    eprintln!("phase {name}: {:.1} s", millis as f64 / 1000.0);
    if let Ok(mut phases) = PHASES.lock() {
        phases.push((name.to_owned(), millis));
    }
}

/// V2 separates actual elapsed operation time from inclusive, possibly nested
/// phase durations. Historical unversioned total_ms was only a phase sum.
pub(in crate::local_workflow) fn timing_record(
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
pub(in crate::local_workflow) fn finish_phases(
    root: &Path,
    command: &str,
    outcome: &str,
    elapsed: std::time::Duration,
) {
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
