//! Run a gate's Cargo test binaries concurrently, each in its own private home.
//!
//! `cargo test` builds every test binary of the selected packages, then runs
//! them one after another. On the Wave 10 ship of 2026-11-07 that serial run
//! was 574 s of the 784 s test gate while 124 of its 173 binaries finished in
//! under a second: the wall clock was a queue, not work. `cargo nextest` runs
//! the tests of one binary in separate processes and so defeats the
//! in-process locks (`ENV_LOCK` and friends) the suites use to serialize
//! their shared state; 2 of its 3 ships on main failed that way.
//!
//! This runner keeps every binary whole (its in-process locks keep working)
//! and runs binaries beside each other, each with its own HOME, APPDATA,
//! LOCALAPPDATA, XDG and SUSI_HOME trees, so two binaries never meet in a
//! daemon workspace, a config file or an audit log. The build stays Cargo's:
//! the planned `cargo test` arguments run with `--no-run
//! --message-format=json`, and the executables Cargo reports are launched
//! the way Cargo launches them (manifest directory as cwd, the `CARGO_*`
//! runtime variables, the target directories on `PATH`).
//!
//! Every binary's output is appended to the gate log as one block under the
//! exact `Running ...` header Cargo prints, so the pass counters
//! (`workspace_test_count`, `native_test_counts`) read the log unchanged.
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// `SUSI_LOCAL_TEST_RUNNER=serial` keeps the one-after-another `cargo test`
/// run; anything else (the default) runs binaries concurrently. Fix a test
/// that only passes serially instead of flipping this: a suite that cannot
/// share a host with another binary cannot share it with another agent's
/// gate either.
pub fn serial_requested() -> bool {
    std::env::var("SUSI_LOCAL_TEST_RUNNER").is_ok_and(|runner| runner == "serial")
}

/// One test executable Cargo built for the gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binary {
    pub package: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub target_name: String,
    /// Cargo's target kind: `lib`, `bin`, `test`, `bench`, `example`, ...
    pub kind: String,
    pub src_path: PathBuf,
    pub executable: PathBuf,
}

impl Binary {
    pub fn manifest_dir(&self) -> &Path {
        self.manifest_path.parent().unwrap_or(&self.manifest_path)
    }

    /// Stable identity across runs (for durations and messages).
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.package, self.kind, self.target_name)
    }

    fn integration(&self) -> bool {
        matches!(self.kind.as_str(), "test" | "bench")
    }

    /// The `Running` line Cargo prints before this binary's output: the
    /// source path relative to the manifest directory, `unittests` for a
    /// library's or binary's own tests, and the executable in parentheses.
    pub fn header(&self) -> String {
        let source = self
            .src_path
            .strip_prefix(self.manifest_dir())
            .unwrap_or(&self.src_path)
            .display();
        if self.integration() {
            format!("     Running {source} ({})", self.executable.display())
        } else {
            format!(
                "     Running unittests {source} ({})",
                self.executable.display()
            )
        }
    }
}

/// Package name and version from a Cargo package id
/// (`path+file:///x/crates/name#1.2.3` or `path+file:///x/dir#name@1.2.3`).
fn package_identity(id: &str, manifest_path: &Path) -> Result<(String, String)> {
    let (location, fragment) = id
        .rsplit_once('#')
        .ok_or_else(|| format!("package id without version: {id}"))?;
    if let Some((name, version)) = fragment.split_once('@') {
        return Ok((name.to_owned(), version.to_owned()));
    }
    let name = location
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("package id without name: {id}"))?;
    // A path id names the package after its directory only when they agree;
    // confirm against the manifest when it is readable so a renamed
    // directory cannot mislabel it.
    let declared = fs::read_to_string(manifest_path)
        .ok()
        .and_then(|manifest| manifest_package_name(&manifest));
    Ok((
        declared.unwrap_or_else(|| name.to_owned()),
        fragment.to_owned(),
    ))
}

fn manifest_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("name") {
                let rest = rest.trim_start();
                if let Some(value) = rest.strip_prefix('=') {
                    return Some(value.trim().trim_matches('"').to_owned());
                }
            }
        }
    }
    None
}

/// The test executables and the rendered compiler diagnostics from the
/// stdout of `cargo test ... --no-run --message-format=json`.
pub fn parse_artifacts(stdout: &str) -> Result<(Vec<Binary>, Vec<String>)> {
    let mut binaries = Vec::new();
    let mut diagnostics = Vec::new();
    for line in stdout.lines() {
        if !line.starts_with('{') {
            continue;
        }
        let message: Value = serde_json::from_str(line)?;
        match message["reason"].as_str() {
            Some("compiler-message") => {
                if let Some(rendered) = message["message"]["rendered"].as_str() {
                    diagnostics.push(rendered.to_owned());
                }
            }
            Some("compiler-artifact") => {
                if message["profile"]["test"] != Value::Bool(true) {
                    continue;
                }
                let Some(executable) = message["executable"].as_str() else {
                    continue;
                };
                let manifest_path = PathBuf::from(
                    message["manifest_path"]
                        .as_str()
                        .ok_or("artifact without manifest path")?,
                );
                let (package, version) = package_identity(
                    message["package_id"]
                        .as_str()
                        .ok_or("artifact without package id")?,
                    &manifest_path,
                )?;
                let target = &message["target"];
                let kind = target["kind"]
                    .as_array()
                    .and_then(|kinds| kinds.first())
                    .and_then(Value::as_str)
                    .ok_or("artifact without target kind")?;
                binaries.push(Binary {
                    package,
                    version,
                    manifest_path,
                    target_name: target["name"]
                        .as_str()
                        .ok_or("artifact without target name")?
                        .to_owned(),
                    kind: kind.to_owned(),
                    src_path: PathBuf::from(
                        target["src_path"]
                            .as_str()
                            .ok_or("artifact without source path")?,
                    ),
                    executable: PathBuf::from(executable),
                });
            }
            _ => {}
        }
    }
    binaries.sort_by_key(Binary::id);
    binaries.dedup();
    Ok((binaries, diagnostics))
}

/// Binary target names that `.config/nextest.toml` reserves two threads for
/// (`binary(name)` in its heavy filter): they spawn `susi` sessions with
/// wall-clock deadlines, so each takes two of the runner's slots.
pub fn heavy_binaries(root: &Path) -> Vec<String> {
    let Ok(config) = fs::read_to_string(root.join(".config/nextest.toml")) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut rest = config.as_str();
    while let Some(at) = rest.find("binary(") {
        rest = &rest[at + "binary(".len()..];
        if let Some(end) = rest.find(')') {
            let name = rest[..end].trim();
            if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                names.push(name.to_owned());
            }
            rest = &rest[end..];
        }
    }
    names.sort();
    names.dedup();
    names
}

/// How many binaries run beside each other for a gate that may use `jobs`
/// build jobs: a quarter of them, between 2 and 6. The suites spawn `susi`
/// processes with deadlines and a few share a target directory through the
/// cargo they run themselves, so more lanes stop paying off well before the
/// core count does. Measured 2026-11-07 on the 28-core CEO laptop, same 129
/// binaries (root + xtask) back to back: serial `cargo test` 795 s, six
/// lanes of four threads 476 s. Giving the four longest binaries every
/// core instead made it 556 s: they spawn cargo and git, and four of them
/// at 28 threads each starve the host.
pub fn concurrency(jobs: usize) -> usize {
    (jobs / 4).clamp(2, 6)
}

/// Legacy per-binary estimate retained for callers migrating to raw budgets.
pub fn threads_per_binary(test_threads: usize, concurrency: usize) -> usize {
    (test_threads / concurrency.max(1)).max(2)
}

pub struct Settings<'a> {
    pub concurrency: usize,
    /// Raw aggregate libtest-thread budget shared by concurrent binaries.
    pub total_test_threads: usize,
    /// `CARGO_BUILD_JOBS` for the cargo invocations some suites run
    /// themselves (xtask's tests build scratch workspaces): the gate's jobs
    /// divided among the lanes, so six lanes do not each claim every core.
    pub build_jobs: usize,
    /// Each binary gets `<home_root>/<n>-<target>` as its HOME tree.
    pub home_root: &'a Path,
    pub target_dir: &'a Path,
    /// The `--target` triple of the build, when it had one: the profile
    /// directory is then `<target_dir>/<triple>/debug`, else `<target_dir>/debug`.
    pub triple: Option<&'a str>,
    /// Measured milliseconds per binary id, read for scheduling and
    /// rewritten after the run.
    pub durations: &'a Path,
    pub heavy: &'a [String],
    /// The gate log every binary's block is appended to.
    pub log: &'a Path,
}

#[derive(Debug)]
pub struct Outcome {
    pub ran: usize,
    /// Binary ids with their exit codes, for every binary that did not exit 0.
    pub failed: Vec<(String, Option<i32>)>,
    pub elapsed_ms: u128,
}

/// The variables Cargo sets when it runs a test binary (the compile-time
/// `env!` ones are already inside the executable), plus the target
/// directories on `PATH` for any dynamic library beside the binaries.
fn cargo_runtime_env(
    command: &mut Command,
    binary: &Binary,
    settings: &Settings<'_>,
    test_threads: usize,
) {
    let profile_dir = match settings.triple {
        Some(triple) => settings.target_dir.join(triple).join("debug"),
        None => settings.target_dir.join("debug"),
    };
    let mut path = std::ffi::OsString::new();
    for dir in [profile_dir.join("deps"), profile_dir.clone()] {
        path.push(dir.as_os_str());
        path.push(if cfg!(windows) { ";" } else { ":" });
    }
    if let Some(inherited) = std::env::var_os("PATH") {
        path.push(inherited);
    }
    let version = binary.version.as_str();
    let mut parts = version.split(['.', '-', '+']);
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    let patch = parts.next().unwrap_or("0");
    let pre = version
        .split_once('-')
        .map(|(_, pre)| pre.split('+').next().unwrap_or(""))
        .unwrap_or("");
    command
        .current_dir(binary.manifest_dir())
        .env("PATH", path)
        .env("CARGO", cargo_executable())
        .env("CARGO_MANIFEST_DIR", binary.manifest_dir())
        .env("CARGO_MANIFEST_PATH", &binary.manifest_path)
        .env("CARGO_PKG_NAME", &binary.package)
        .env("CARGO_PKG_VERSION", version)
        .env("CARGO_PKG_VERSION_MAJOR", major)
        .env("CARGO_PKG_VERSION_MINOR", minor)
        .env("CARGO_PKG_VERSION_PATCH", patch)
        .env("CARGO_PKG_VERSION_PRE", pre)
        .env("CARGO_CRATE_NAME", binary.target_name.replace('-', "_"))
        .env("CARGO_PRIMARY_PACKAGE", "1")
        .env("CARGO_BUILD_JOBS", settings.build_jobs.to_string())
        .env("RUST_TEST_THREADS", test_threads.to_string());
    if binary.kind == "bin" {
        command.env("CARGO_BIN_NAME", &binary.target_name);
    }
    if binary.integration() {
        let tmp = settings.target_dir.join("tmp");
        let _ = fs::create_dir_all(&tmp);
        command.env("CARGO_TARGET_TMPDIR", tmp);
    }
}

fn cargo_executable() -> PathBuf {
    if let Some(cargo) = std::env::var_os("CARGO") {
        return PathBuf::from(cargo);
    }
    let name = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn load_durations(path: &Path) -> BTreeMap<String, u64> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, u64>>(&bytes).ok())
        .unwrap_or_default()
}

/// Longest first, by the last measured duration; binaries never measured
/// go first (an unmeasured binary is more often a long one than a short
/// one, and a wrong guess only costs ordering).
fn schedule(binaries: &[Binary], durations: &BTreeMap<String, u64>) -> VecDeque<Binary> {
    let mut order: Vec<&Binary> = binaries.iter().collect();
    order.sort_by_key(|binary| {
        std::cmp::Reverse(durations.get(&binary.id()).copied().unwrap_or(u64::MAX))
    });
    order.into_iter().cloned().collect()
}

/// The pending binaries (with their lane weights, already clamped to the
/// pool) and the free lanes, kept under one lock so choosing a binary and
/// reserving its lanes is a single step.
struct SchedulerState {
    pending: VecDeque<(Binary, usize)>,
    free: usize,
}

impl SchedulerState {
    /// Remove the earliest pending binary whose weight fits the free lanes
    /// and reserve them. Binaries that fit keep their queue order; one that
    /// does not fit stays where it is. Finite first fit: a heavy binary may
    /// wait while lighter ones behind it fill the free lanes.
    fn take_fitting(&mut self) -> Option<(Binary, usize)> {
        let free = self.free;
        let position = self
            .pending
            .iter()
            .position(|(_, weight)| *weight <= free)?;
        let (binary, weight) = self.pending.remove(position)?;
        self.free = free.saturating_sub(weight);
        Some((binary, weight))
    }
}

/// Hands each worker the earliest pending binary that fits the free lanes,
/// so a worker never holds a binary while lanes it cannot use sit idle.
struct Scheduler {
    state: Mutex<SchedulerState>,
    changed: Condvar,
    /// Set (under the state lock) once a binary fails or a worker unwinds:
    /// no further binary starts.
    stop: AtomicBool,
}

impl Scheduler {
    fn new(order: VecDeque<Binary>, heavy: &[String], lanes: usize) -> Self {
        let lanes = lanes.max(1);
        let pending = order
            .into_iter()
            .map(|binary| {
                // Every binary holds one lane; a heavy one holds two so it
                // keeps the CPU its deadlines assume. The lane count stays
                // the limit on what runs at once.
                let lanes_needed = weight(&binary, heavy).min(lanes);
                (binary, lanes_needed)
            })
            .collect();
        Self {
            state: Mutex::new(SchedulerState {
                pending,
                free: lanes,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Wait for the earliest pending binary that fits the free lanes and
    /// reserve them. `None` once the run is stopped or nothing is pending.
    /// No binary leaves the queue while its worker waits.
    fn reserve(&self) -> Result<Option<(Binary, Reservation<'_>)>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "runner scheduler state poisoned")?;
        loop {
            if self.stopped() || state.pending.is_empty() {
                return Ok(None);
            }
            if let Some((binary, weight)) = state.take_fitting() {
                return Ok(Some((
                    binary,
                    Reservation {
                        scheduler: self,
                        weight,
                    },
                )));
            }
            state = self
                .changed
                .wait(state)
                .map_err(|_| "runner scheduler state poisoned")?;
        }
    }

    /// Stop starting binaries and wake every waiting worker. The flag is set
    /// under the state lock, so a worker between its stop check and its wait
    /// cannot miss the wakeup. A poisoned lock is recovered only to stop and
    /// wake; `reserve` still reports the poison.
    fn cancel(&self) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.stop.store(true, Ordering::SeqCst);
        drop(state);
        self.changed.notify_all();
    }
}

/// Lanes reserved for one binary; returned to the pool when dropped, on
/// every path including errors and unwinding.
struct Reservation<'a> {
    scheduler: &'a Scheduler,
    weight: usize,
}

impl Reservation<'_> {
    fn weight(&self) -> usize {
        self.weight
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        // Recover a poisoned lock only to return the lanes and wake waiters.
        let mut state = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.free = state.free.saturating_add(self.weight);
        if std::thread::panicking() {
            // A worker unwinding out of a launch fails the run: start
            // nothing more.
            self.scheduler.stop.store(true, Ordering::SeqCst);
        }
        drop(state);
        self.scheduler.changed.notify_all();
    }
}

/// One finished binary: its id, exit code and elapsed milliseconds.
type Finished = (String, Option<i32>, u64);

struct Shared<'a> {
    scheduler: Scheduler,
    log: Mutex<fs::File>,
    results: Mutex<Vec<Finished>>,
    settings: &'a Settings<'a>,
}

fn weight(binary: &Binary, heavy: &[String]) -> usize {
    if is_local_workflow(binary) || heavy.contains(&binary.target_name) {
        2
    } else {
        1
    }
}

fn is_local_workflow(binary: &Binary) -> bool {
    binary.package == "xtask" && binary.kind == "bin" && binary.target_name == "local-workflow"
}

/// Effective test threads for a binary holding `weight` scheduler lanes.
/// The workflow runner is the one exact target assigned two proportional
/// units. Other binaries (including unrelated targets with the same name)
/// stay at one unit even if nextest marks them heavy. Since units per binary
/// never exceed the acquired lane weight, total assignment is bounded by the
/// aggregate raw budget whenever active weights fit within the lane pool.
fn threads_for_binary(
    binary: &Binary,
    budget: usize,
    lanes: usize,
    acquired_weight: usize,
) -> usize {
    let per_lane = (budget.max(1) / lanes.max(1)).max(1);
    let units = if is_local_workflow(binary) {
        acquired_weight.max(1)
    } else {
        1
    };
    per_lane.saturating_mul(units)
}

fn effective_lanes(concurrency: usize, total_test_threads: usize) -> usize {
    concurrency.max(1).min(total_test_threads.max(1))
}

fn run_one(
    shared: &Shared<'_>,
    index: usize,
    binary: &Binary,
    resources: (usize, usize),
    launch: &(dyn Fn(&Binary, &Path) -> Result<Command> + Sync),
) -> Result<()> {
    let (acquired_weight, effective_lanes) = resources;
    let settings = shared.settings;
    let home = settings
        .home_root
        .join(format!("{index}-{}", binary.target_name));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join("AppData/Local"))?;
    let capture_path = settings
        .home_root
        .join(format!("{index}-{}.log", binary.target_name));
    let capture = fs::File::create(&capture_path)?;
    let mut command = launch(binary, &home)?;
    let test_threads = threads_for_binary(
        binary,
        settings.total_test_threads,
        effective_lanes,
        acquired_weight,
    );
    // Keep Cargo's nested-build budget unchanged. Only the actual libtest
    // thread allocation follows the scheduler lanes acquired by this binary.
    cargo_runtime_env(&mut command, binary, settings, test_threads);
    command
        .stdout(Stdio::from(capture.try_clone()?))
        .stderr(Stdio::from(capture));
    let started = Instant::now();
    let status = command.status();
    let elapsed = started.elapsed().as_millis();
    let (code, note) = match &status {
        Ok(status) => (status.code(), None),
        Err(error) => (None, Some(format!("could not start: {error}"))),
    };
    let output = fs::read(&capture_path).unwrap_or_default();
    {
        let mut log = shared.log.lock().map_err(|_| "gate log poisoned")?;
        writeln!(log, "{}", binary.header())?;
        log.write_all(&output)?;
        if !output.ends_with(b"\n") {
            writeln!(log)?;
        }
        match (code, note) {
            (Some(0), None) => {}
            (code, note) => writeln!(
                log,
                "runner: {} exited with {} after {:.1} s{}",
                binary.id(),
                code.map_or_else(|| "no exit code".to_owned(), |c| format!("code {c}")),
                elapsed as f64 / 1000.0,
                note.map_or_else(String::new, |n| format!(" ({n})"))
            )?,
        }
        log.flush()?;
    }
    let _ = fs::remove_file(&capture_path);
    let _ = fs::remove_dir_all(&home);
    shared
        .results
        .lock()
        .map_err(|_| "runner results poisoned")?
        .push((
            binary.id(),
            code,
            u64::try_from(elapsed).unwrap_or(u64::MAX),
        ));
    if code != Some(0) {
        shared.scheduler.cancel();
    }
    Ok(())
}

/// Run `binaries` with up to the effective test-thread budget's lanes busy.
/// `launch(binary, home)` returns the command for one binary with the gate's
/// environment and its private `home`; the runner adds Cargo's runtime
/// variables, the working directory and the output capture. After the first
/// failure no further binary starts; the ones already running finish and are
/// recorded. The gate log gains one block per binary that ran.
pub fn run(
    binaries: &[Binary],
    settings: &Settings<'_>,
    launch: &(dyn Fn(&Binary, &Path) -> Result<Command> + Sync),
) -> Result<Outcome> {
    fs::create_dir_all(settings.home_root)?;
    let durations = load_durations(settings.durations);
    let log = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(settings.log)?;
    let total_test_threads = settings.total_test_threads.max(1);
    let lanes = effective_lanes(settings.concurrency, total_test_threads);
    let shared = Shared {
        scheduler: Scheduler::new(schedule(binaries, &durations), settings.heavy, lanes),
        log: Mutex::new(log),
        results: Mutex::new(Vec::new()),
        settings,
    };
    let started = Instant::now();
    let workers = lanes.min(binaries.len().max(1));
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let shared = &shared;
            let errors = &errors;
            scope.spawn(move || {
                let mut index = worker;
                loop {
                    // The earliest pending binary that fits the free lanes,
                    // with those lanes already reserved: a light binary
                    // behind heavy ones fills a lane no heavy one can use.
                    let (binary, reservation) = match shared.scheduler.reserve() {
                        Ok(Some(next)) => next,
                        Ok(None) => break,
                        Err(error) => {
                            if let Ok(mut errors) = errors.lock() {
                                errors.push(format!("scheduler: {error}"));
                            }
                            shared.scheduler.cancel();
                            break;
                        }
                    };
                    // Another binary may have failed since the reservation.
                    // Do not launch after that failure; dropping the
                    // reservation returns its lanes.
                    if shared.scheduler.stopped() {
                        drop(reservation);
                        break;
                    }
                    let acquired_weight = reservation.weight();
                    let assigned_threads = threads_for_binary(
                        &binary,
                        shared.settings.total_test_threads,
                        lanes,
                        acquired_weight,
                    );
                    if let Err(error) = run_one(
                        shared,
                        index,
                        &binary,
                        (acquired_weight, lanes),
                        launch,
                    ) {
                        if let Ok(mut errors) = errors.lock() {
                            errors.push(format!("{}: {error}", binary.id()));
                        }
                        shared.scheduler.cancel();
                    }
                    drop(reservation);
                    if let Ok(mut log) = shared.log.lock() {
                        let _ = writeln!(
                            log,
                            "runner: {} assigned {acquired_weight}/{lanes} lanes and {assigned_threads}/{} test threads",
                            binary.id(),
                            shared.settings.total_test_threads.max(1)
                        );
                        let _ = log.flush();
                    }
                    index += workers;
                }
            });
        }
    });
    let errors = errors.into_inner().map_err(|_| "runner errors poisoned")?;
    if let Some(error) = errors.first() {
        return Err(format!("test runner: {error}").into());
    }
    let results = shared
        .results
        .into_inner()
        .map_err(|_| "runner results poisoned")?;
    let mut measured = durations;
    for (id, _, elapsed) in &results {
        measured.insert(id.clone(), *elapsed);
    }
    if let Some(parent) = settings.durations.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(settings.durations, serde_json::to_vec_pretty(&measured)?)?;
    let mut failed: Vec<(String, Option<i32>)> = results
        .iter()
        .filter(|(_, code, _)| *code != Some(0))
        .map(|(id, code, _)| (id.clone(), *code))
        .collect();
    failed.sort();
    let elapsed_ms = started.elapsed().as_millis();
    let outcome = Outcome {
        ran: results.len(),
        failed,
        elapsed_ms,
    };
    let mut log = shared.log.into_inner().map_err(|_| "gate log poisoned")?;
    writeln!(
        log,
        "runner: {} of {} binaries ran on {} lanes in {:.1} s; {} failed{}",
        outcome.ran,
        binaries.len(),
        lanes,
        elapsed_ms as f64 / 1000.0,
        outcome.failed.len(),
        if outcome.ran < binaries.len() {
            " (stopped at the first failure)"
        } else {
            ""
        }
    )?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary(package: &str, kind: &str, name: &str) -> Binary {
        let manifest = if package == "root" {
            PathBuf::from("/ws/Cargo.toml")
        } else {
            PathBuf::from(format!("/ws/crates/{package}/Cargo.toml"))
        };
        let src = if kind == "test" {
            manifest
                .parent()
                .unwrap()
                .join("tests")
                .join(format!("{name}.rs"))
        } else {
            manifest.parent().unwrap().join("src/lib.rs")
        };
        Binary {
            package: package.into(),
            version: "0.21.51".into(),
            manifest_path: manifest,
            target_name: name.into(),
            kind: kind.into(),
            src_path: src,
            executable: PathBuf::from(format!("/ws/target/x/debug/deps/{name}-abc.exe")),
        }
    }

    #[test]
    fn headers_match_cargo_for_unit_and_integration_binaries() {
        let lib = binary("susi-harness", "lib", "susi-harness");
        let relative = |binary: &Binary| {
            binary
                .src_path
                .strip_prefix(binary.manifest_dir())
                .unwrap()
                .display()
                .to_string()
        };
        assert_eq!(
            lib.header(),
            format!(
                "     Running unittests {} ({})",
                relative(&lib),
                lib.executable.display()
            )
        );
        assert!(relative(&lib).ends_with("lib.rs"), "{}", relative(&lib));
        let it = binary("root", "test", "gmcp_requirements");
        assert_eq!(
            it.header(),
            format!(
                "     Running {} ({})",
                relative(&it),
                it.executable.display()
            )
        );
        assert!(
            relative(&it).ends_with("gmcp_requirements.rs") && !relative(&it).starts_with('/'),
            "{}",
            relative(&it)
        );
    }

    #[test]
    fn artifacts_keep_only_test_executables() {
        let stdout = concat!(
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///ws/crates/susi-harness#0.21.51\",",
            "\"manifest_path\":\"/ws/crates/susi-harness/Cargo.toml\",\"target\":{\"kind\":[\"lib\"],\"name\":\"susi-harness\",",
            "\"src_path\":\"/ws/crates/susi-harness/src/lib.rs\"},\"profile\":{\"test\":true},\"executable\":\"/ws/t/deps/susi_harness-1.exe\"}\n",
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///ws/crates/susi-core#0.21.51\",",
            "\"manifest_path\":\"/ws/crates/susi-core/Cargo.toml\",\"target\":{\"kind\":[\"lib\"],\"name\":\"susi-core\",",
            "\"src_path\":\"/ws/crates/susi-core/src/lib.rs\"},\"profile\":{\"test\":false},\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///ws/susi-agent-x#susi@0.21.51\",",
            "\"manifest_path\":\"/ws/Cargo.toml\",\"target\":{\"kind\":[\"test\"],\"name\":\"tasks_cli\",",
            "\"src_path\":\"/ws/tests/tasks_cli.rs\"},\"profile\":{\"test\":true},\"executable\":\"/ws/t/deps/tasks_cli-2.exe\"}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"rendered\":\"warning: something\\n\"}}\n",
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        );
        let (binaries, diagnostics) = parse_artifacts(stdout).unwrap();
        assert_eq!(
            binaries
                .iter()
                .map(|b| (b.package.as_str(), b.kind.as_str(), b.target_name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("susi-harness", "lib", "susi-harness"),
                ("susi", "test", "tasks_cli")
            ]
        );
        assert_eq!(binaries[0].version, "0.21.51");
        assert_eq!(diagnostics, vec!["warning: something\n"]);
    }

    #[test]
    fn package_identity_reads_the_manifest_when_the_id_omits_the_name() {
        let dir = std::env::temp_dir().join(format!("susi-runner-id-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"renamed\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let (name, version) = package_identity("path+file:///x/somedir#1.0.0", &manifest).unwrap();
        assert_eq!((name.as_str(), version.as_str()), ("renamed", "1.0.0"));
        let (name, version) =
            package_identity("path+file:///x/somedir#other@2.0.0", &manifest).unwrap();
        assert_eq!((name.as_str(), version.as_str()), ("other", "2.0.0"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn heavy_binaries_come_from_the_nextest_reservation() {
        let dir = std::env::temp_dir().join(format!("susi-runner-heavy-{}", std::process::id()));
        fs::create_dir_all(dir.join(".config")).unwrap();
        fs::write(
            dir.join(".config/nextest.toml"),
            "[[profile.default.overrides]]\nfilter = 'binary(gmcp_requirements) | binary(worktree_target_dir) | (package(susi-gawd) & test(/x/))'\nthreads-required = 2\n",
        )
        .unwrap();
        assert_eq!(
            heavy_binaries(&dir),
            vec![
                "gmcp_requirements".to_owned(),
                "worktree_target_dir".to_owned()
            ]
        );
        fs::remove_dir_all(dir).unwrap();
        assert!(heavy_binaries(Path::new("/nonexistent-susi-runner")).is_empty());
    }

    #[test]
    fn lanes_and_threads_scale_with_the_gate_share() {
        assert_eq!(concurrency(28), 6);
        assert_eq!(concurrency(14), 3);
        assert_eq!(concurrency(2), 2);
        assert_eq!(threads_per_binary(28, 6), 4);
        assert_eq!(threads_per_binary(2, 6), 2);
    }

    #[test]
    fn aggregate_thread_allocations_follow_exact_identity_and_small_budgets() {
        let workflow = binary("xtask", "bin", "local-workflow");
        let same_name_other_package = binary("other", "bin", "local-workflow");
        assert_eq!(weight(&workflow, &[]), 2);
        assert_eq!(weight(&same_name_other_package, &[]), 1);

        assert_eq!(effective_lanes(6, 3), 3);
        assert_eq!(threads_for_binary(&workflow, 3, 3, 2), 2);
        assert_eq!(threads_for_binary(&binary("a", "lib", "a"), 3, 3, 1), 1);
        assert_eq!(threads_for_binary(&same_name_other_package, 3, 3, 1), 1);

        assert_eq!(effective_lanes(6, 1), 1);
        assert_eq!(threads_for_binary(&workflow, 1, 1, 1), 1);
        assert_eq!(threads_for_binary(&binary("a", "lib", "a"), 1, 1, 1), 1);

        // For any active set whose acquired weights fit within the pool,
        // allocated test-thread units are no greater than total lane weight.
        for budget in 1..=12 {
            let lanes = effective_lanes(6, budget);
            let workflow_weight = weight(&workflow, &[]).min(lanes);
            let ordinary = binary("ordinary", "lib", "ordinary");
            let ordinary_weight = weight(&ordinary, &[]).min(lanes);
            let mut used_lanes = workflow_weight;
            let mut assigned = threads_for_binary(&workflow, budget, lanes, workflow_weight);
            while used_lanes + ordinary_weight <= lanes {
                used_lanes += ordinary_weight;
                assigned += threads_for_binary(&ordinary, budget, lanes, ordinary_weight);
            }
            assert!(
                assigned <= budget,
                "budget={budget}, lanes={lanes}, assigned={assigned}"
            );
        }
    }

    fn free_lanes(scheduler: &Scheduler) -> usize {
        scheduler.state.lock().unwrap().free
    }

    fn pending_names(scheduler: &Scheduler) -> Vec<String> {
        scheduler
            .state
            .lock()
            .unwrap()
            .pending
            .iter()
            .map(|(binary, _)| binary.target_name.clone())
            .collect()
    }

    /// `reserve` mapped to sendable values; the reservation is returned
    /// before the result leaves the thread.
    fn reserve_name(scheduler: &Scheduler) -> std::result::Result<Option<String>, String> {
        scheduler
            .reserve()
            .map(|next| next.map(|(binary, _reservation)| binary.target_name))
            .map_err(|error| error.to_string())
    }

    #[test]
    fn effective_lane_pool_bounds_mixed_weights() {
        use std::sync::mpsc;
        use std::time::Duration;

        let workflow = binary("xtask", "bin", "local-workflow");
        let ordinary = binary("ordinary", "lib", "ordinary");
        let budget = 3;
        let lanes = effective_lanes(6, budget);
        let scheduler = Scheduler::new(
            VecDeque::from(vec![
                workflow.clone(),
                ordinary.clone(),
                binary("third", "lib", "third"),
            ]),
            &[],
            lanes,
        );
        let (first, workflow_lanes) = scheduler.reserve().unwrap().unwrap();
        let (second, ordinary_lanes) = scheduler.reserve().unwrap().unwrap();
        assert_eq!(
            (first.target_name.as_str(), second.target_name.as_str()),
            ("local-workflow", "ordinary")
        );
        assert_eq!(workflow_lanes.weight(), weight(&workflow, &[]).min(lanes));
        assert_eq!(ordinary_lanes.weight(), weight(&ordinary, &[]).min(lanes));
        assert_eq!(free_lanes(&scheduler), 0);
        let (acquired_tx, acquired_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = acquired_tx.send(reserve_name(&scheduler));
            });
            let premature = acquired_rx.recv_timeout(Duration::from_millis(25)).ok();
            let assigned = threads_for_binary(&workflow, budget, lanes, workflow_lanes.weight())
                + threads_for_binary(&ordinary, budget, lanes, ordinary_lanes.weight());
            // Release before assertions so a failing regression never
            // strands the waiter inside the scoped join.
            drop(workflow_lanes);
            let acquired = acquired_rx.recv_timeout(Duration::from_secs(5));
            assert!(premature.is_none(), "{premature:?}");
            assert_eq!(assigned, budget);
            assert_eq!(acquired, Ok(Ok(Some("third".to_owned()))));
        });
        drop(ordinary_lanes);
        assert_eq!(free_lanes(&scheduler), lanes);
    }

    #[test]
    fn failure_cancels_a_dequeued_binary_waiting_for_weighted_lanes() {
        use std::sync::mpsc;
        use std::time::Duration;

        // The waiting binary is no longer dequeued before its lanes are free:
        // it stays pending, and a failure must still keep it from launching.
        let heavy = vec!["waiting".to_owned()];
        let scheduler = Scheduler::new(
            VecDeque::from(vec![
                binary("xtask", "bin", "local-workflow"),
                binary("p", "test", "waiting"),
            ]),
            &heavy,
            2,
        );
        let (held, held_reservation) = scheduler.reserve().unwrap().unwrap();
        assert_eq!(held.target_name, "local-workflow");
        assert_eq!(free_lanes(&scheduler), 0);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = entered_tx.send(());
                let _ = result_tx.send(reserve_name(&scheduler));
            });
            let entered = entered_rx.recv_timeout(Duration::from_secs(1));
            let premature = result_rx.recv_timeout(Duration::from_millis(25)).ok();
            // Cancellation alone (no lane released) must wake the waiter.
            scheduler.cancel();
            let woken = result_rx.recv_timeout(Duration::from_secs(5));
            // Release before assertions so a failing regression never strands
            // a worker inside the scoped thread join.
            drop(held_reservation);
            assert!(entered.is_ok());
            assert!(premature.is_none(), "{premature:?}");
            assert_eq!(
                woken,
                Ok(Ok(None)),
                "a binary must not launch after a waiting gate fails"
            );
        });
        assert_eq!(free_lanes(&scheduler), 2);
        assert_eq!(pending_names(&scheduler), vec!["waiting"]);
        scheduler.stop.store(false, Ordering::SeqCst);
        let (again, reservation) = scheduler.reserve().unwrap().unwrap();
        assert_eq!(again.target_name, "waiting");
        assert_eq!(reservation.weight(), 2);
        assert_eq!(free_lanes(&scheduler), 0);
        drop(reservation);
        assert_eq!(free_lanes(&scheduler), 2);
    }

    #[test]
    fn scheduler_takes_the_earliest_fitting_binary_and_keeps_fifo_among_fits() {
        fn take(state: &mut SchedulerState) -> Option<(String, usize)> {
            state
                .take_fitting()
                .map(|(binary, weight)| (binary.target_name, weight))
        }
        fn names(state: &SchedulerState) -> Vec<String> {
            state
                .pending
                .iter()
                .map(|(binary, _)| binary.target_name.clone())
                .collect()
        }
        let heavy = vec!["h1".to_owned(), "h2".to_owned()];
        let scheduler = Scheduler::new(
            ["h1", "h2", "l1", "l2"]
                .iter()
                .map(|name| binary("p", "lib", name))
                .collect(),
            &heavy,
            3,
        );
        let mut state = scheduler.state.lock().unwrap();
        assert_eq!(take(&mut state), Some(("h1".to_owned(), 2)));
        assert_eq!(state.free, 1);
        // h2 does not fit the one free lane; l1 backfills it.
        assert_eq!(take(&mut state), Some(("l1".to_owned(), 1)));
        assert_eq!(state.free, 0);
        // Nothing fits: nothing leaves the queue.
        assert_eq!(take(&mut state), None);
        assert_eq!(names(&state), vec!["h2", "l2"]);
        state.free += 1; // l1 finished
        assert_eq!(take(&mut state), Some(("l2".to_owned(), 1)));
        assert_eq!(take(&mut state), None);
        assert_eq!(names(&state), vec!["h2"]);
        state.free += 3; // h1 and l2 finished
        assert_eq!(take(&mut state), Some(("h2".to_owned(), 2)));
        assert!(state.pending.is_empty());
        assert_eq!(state.free, 1);
        drop(state);

        // A weight never exceeds the pool, so the head always fits an idle pool.
        let single = Scheduler::new(VecDeque::from(vec![binary("p", "lib", "h1")]), &heavy, 1);
        let (_, reservation) = single.reserve().unwrap().unwrap();
        assert_eq!(reservation.weight(), 1);
        assert_eq!(free_lanes(&single), 0);
        drop(reservation);
        assert_eq!(free_lanes(&single), 1);
    }

    #[test]
    fn scheduler_reservations_restore_weight_and_respect_cancellation() {
        let heavy = vec!["h1".to_owned()];
        let scheduler = Scheduler::new(
            ["h1", "l1", "l2"]
                .iter()
                .map(|name| binary("p", "lib", name))
                .collect(),
            &heavy,
            3,
        );
        let (h1, h1_lanes) = scheduler.reserve().unwrap().unwrap();
        let (l1, l1_lanes) = scheduler.reserve().unwrap().unwrap();
        assert_eq!((h1.target_name.as_str(), h1_lanes.weight()), ("h1", 2));
        assert_eq!((l1.target_name.as_str(), l1_lanes.weight()), ("l1", 1));
        assert_eq!(free_lanes(&scheduler), 0);
        drop(h1_lanes);
        assert_eq!(free_lanes(&scheduler), 2);
        let (l2, l2_lanes) = scheduler.reserve().unwrap().unwrap();
        assert_eq!(l2.target_name, "l2");
        assert_eq!(free_lanes(&scheduler), 1);
        drop(l1_lanes);
        drop(l2_lanes);
        assert_eq!(free_lanes(&scheduler), 3);
        assert!(scheduler.reserve().unwrap().is_none());

        let canceled = Scheduler::new(VecDeque::from(vec![binary("p", "lib", "l1")]), &heavy, 3);
        canceled.cancel();
        assert!(canceled.stopped());
        assert!(canceled.reserve().unwrap().is_none());
        assert_eq!(pending_names(&canceled), vec!["l1"]);
        assert_eq!(free_lanes(&canceled), 3);
    }

    #[test]
    fn reservations_return_lanes_when_a_launch_errors_or_panics() {
        /// A launch that fails while its lanes are reserved.
        fn failing_launch(scheduler: &Scheduler) -> Result<()> {
            let (_binary, reservation) = scheduler.reserve()?.ok_or("nothing to reserve")?;
            Err(format!(
                "launch callback failed holding {} lanes",
                reservation.weight()
            )
            .into())
        }
        let scheduler = Scheduler::new(
            VecDeque::from(vec![
                binary("xtask", "bin", "local-workflow"),
                binary("p", "lib", "after-error"),
            ]),
            &[],
            3,
        );
        assert_eq!(
            failing_launch(&scheduler).map_err(|error| error.to_string()),
            Err("launch callback failed holding 2 lanes".to_owned())
        );
        assert_eq!(free_lanes(&scheduler), 3);
        assert!(!scheduler.stopped());

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(Some((_binary, _reservation))) = scheduler.reserve() {
                panic!("launch callback panicked while holding lanes");
            }
        }));
        assert!(
            unwound.is_err(),
            "the launch must have panicked while holding its reservation"
        );
        assert!(
            scheduler.state.lock().is_ok(),
            "the state lock was poisoned"
        );
        assert_eq!(free_lanes(&scheduler), 3);
        assert!(
            scheduler.stopped(),
            "an unwinding launch stops further binaries"
        );
        assert!(scheduler.reserve().unwrap().is_none());
    }

    #[test]
    fn poisoned_scheduler_reports_errors_and_releases_waiters_and_lanes() {
        use std::sync::mpsc;
        use std::time::Duration;

        let scheduler = Scheduler::new(
            VecDeque::from(vec![
                binary("xtask", "bin", "local-workflow"),
                binary("p", "lib", "waiting"),
            ]),
            &[],
            2,
        );
        let (_, reservation) = scheduler.reserve().unwrap().unwrap();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = result_tx.send(reserve_name(&scheduler));
            });
            let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _state = scheduler.state.lock().unwrap();
                panic!("poison the scheduler state");
            }));
            scheduler.cancel();
            let result = result_rx.recv_timeout(Duration::from_secs(5));
            // Return the held lanes and wake before any assertion or join.
            drop(reservation);
            assert!(poisoned.is_err());
            assert_eq!(
                result,
                Ok(Err("runner scheduler state poisoned".to_owned()))
            );
        });
        assert!(scheduler.stopped());
        assert_eq!(
            reserve_name(&scheduler),
            Err("runner scheduler state poisoned".to_owned())
        );
        let state = scheduler
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(state.free, 2);
        assert_eq!(state.pending.len(), 1);
    }

    #[test]
    fn run_reports_a_launch_error_and_releases_its_lanes() {
        let exe = std::env::current_exe().unwrap();
        let dir =
            std::env::temp_dir().join(format!("susi-runner-launch-err-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let make = |name: &str| Binary {
            package: "xtask".into(),
            version: "0.1.0".into(),
            manifest_path: manifest.clone(),
            target_name: name.into(),
            kind: "bin".into(),
            src_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/local-workflow.rs"),
            executable: exe.clone(),
        };
        let settings = Settings {
            concurrency: 3,
            total_test_threads: 3,
            build_jobs: 3,
            home_root: &dir.join("homes"),
            target_dir: &dir.join("target"),
            triple: None,
            durations: &dir.join("durations.json"),
            heavy: &[],
            log: &dir.join("gate.log"),
        };
        let launch = |binary: &Binary, _home: &Path| -> Result<Command> {
            if binary.target_name == "local-workflow" {
                return Err(format!("launch refused for {}", binary.id()).into());
            }
            let mut command = Command::new(&exe);
            command.arg("--list");
            Ok(command)
        };
        let binaries = vec![
            make("local-workflow"),
            make("ordinary-one"),
            make("ordinary-two"),
        ];
        let result = run(&binaries, &settings, &launch).map_err(|error| error.to_string());
        let log = fs::read_to_string(settings.log).unwrap_or_default();
        let _ = fs::remove_dir_all(&dir);
        let error = result.unwrap_err();
        assert!(
            error.contains("test runner: xtask/bin/local-workflow: launch refused"),
            "{error}"
        );
        // Written after the failed launch's reservation was dropped.
        assert!(
            log.contains(
                "runner: xtask/bin/local-workflow assigned 2/3 lanes and 2/3 test threads"
            ),
            "{log}"
        );
    }

    #[test]
    fn run_enforces_the_raw_budget_with_mixed_binary_weights() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("susi-runner-budget-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let make = |name: &str| Binary {
            package: "xtask".into(),
            version: "0.1.0".into(),
            manifest_path: manifest.clone(),
            target_name: name.into(),
            kind: "bin".into(),
            src_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/local-workflow.rs"),
            executable: exe.clone(),
        };
        let binaries = vec![
            make("local-workflow"),
            make("ordinary-one"),
            make("ordinary-two"),
        ];
        let settings = Settings {
            concurrency: 6,
            total_test_threads: 3,
            build_jobs: 6,
            home_root: &dir.join("homes"),
            target_dir: &dir.join("target"),
            triple: None,
            durations: &dir.join("durations.json"),
            heavy: &[],
            log: &dir.join("gate.log"),
        };
        let (entered_tx, entered_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let launch_release = Arc::clone(&release);
        let launch = move |binary: &Binary, _home: &Path| -> Result<Command> {
            entered_tx.send(binary.id()).unwrap();
            let (lock, changed) = &*launch_release;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = changed.wait(ready).unwrap();
            }
            let mut command = Command::new(&exe);
            command.arg("--list");
            Ok(command)
        };

        std::thread::scope(|scope| {
            let running =
                scope.spawn(|| run(&binaries, &settings, &launch).map_err(|e| e.to_string()));
            let mut entered = Vec::new();
            if let Ok(first) = entered_rx.recv_timeout(Duration::from_secs(1)) {
                entered.push(first);
            }
            if let Ok(second) = entered_rx.recv_timeout(Duration::from_secs(1)) {
                entered.push(second);
            }
            let excess_before_release = entered_rx.recv_timeout(Duration::from_millis(25)).ok();

            // Always release workers before asserting, so a regression has
            // a failing test rather than leaving runner threads stranded.
            let (lock, changed) = &*release;
            *lock.lock().unwrap() = true;
            changed.notify_all();
            let outcome = running.join().unwrap().unwrap();

            assert_eq!(outcome.failed.len(), 0, "{outcome:?}");
            assert_eq!(outcome.ran, 3, "{outcome:?}");
            assert_eq!(
                entered.len(),
                2,
                "callbacks that acquired lanes: {entered:?}"
            );
            assert!(
                excess_before_release.is_none(),
                "a third binary entered the callback before any lane was released: {excess_before_release:?}"
            );
        });
        let log = fs::read_to_string(settings.log).unwrap();
        assert!(
            log.contains("runner: 3 of 3 binaries ran on 3 lanes"),
            "{log}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_backfills_a_light_binary_beside_a_heavy_one_on_three_lanes() {
        use std::sync::mpsc;
        use std::time::Duration;

        /// Opens the launch gate when dropped, including while unwinding.
        struct Release<'a>(&'a (Mutex<bool>, Condvar));
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let (lock, changed) = self.0;
                *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
                changed.notify_all();
            }
        }

        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("susi-runner-fill-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let make = |name: &str| Binary {
            package: "xtask".into(),
            version: "0.1.0".into(),
            manifest_path: manifest.clone(),
            target_name: name.into(),
            kind: "bin".into(),
            src_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/local-workflow.rs"),
            executable: exe.clone(),
        };
        let heavy = vec![
            "heavy-one".to_owned(),
            "heavy-two".to_owned(),
            "heavy-three".to_owned(),
        ];
        // No durations file yet: every binary is unmeasured, so the queue
        // keeps this order.
        let binaries = vec![
            make("heavy-one"),
            make("heavy-two"),
            make("heavy-three"),
            make("light"),
        ];
        let settings = Settings {
            concurrency: 3,
            total_test_threads: 3,
            build_jobs: 3,
            home_root: &dir.join("homes"),
            target_dir: &dir.join("target"),
            triple: None,
            durations: &dir.join("durations.json"),
            heavy: &heavy,
            log: &dir.join("gate.log"),
        };
        let gate = (Mutex::new(false), Condvar::new());
        // Each admission with whether the gate was already open, read under
        // the gate lock: a third admission needs a lane returned, which needs
        // a launch to pass the closed gate first.
        let admitted: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());
        let (entered_tx, entered_rx) = mpsc::channel();
        let launch = |binary: &Binary, _home: &Path| -> Result<Command> {
            let (lock, changed) = &gate;
            let mut open = lock.lock().unwrap_or_else(PoisonError::into_inner);
            admitted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((binary.target_name.clone(), *open));
            let _ = entered_tx.send(binary.target_name.clone());
            while !*open {
                open = changed.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
            drop(open);
            let mut command = Command::new(&exe);
            command.arg("--list");
            Ok(command)
        };

        std::thread::scope(|scope| {
            let release = Release(&gate);
            let running =
                scope.spawn(|| run(&binaries, &settings, &launch).map_err(|e| e.to_string()));
            // Hang guards only: both admissions arrive without any timer in
            // the scheduler.
            let first = entered_rx.recv_timeout(Duration::from_secs(60));
            let second = entered_rx.recv_timeout(Duration::from_secs(60));

            // Always release the gate before asserting or joining, so a
            // regression fails instead of stranding runner threads.
            drop(release);
            let outcome = running.join();

            let mut before_release: Vec<String> = admitted
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, open)| !open)
                .map(|(name, _)| name.clone())
                .collect();
            before_release.sort();
            assert!(
                first.is_ok() && second.is_ok(),
                "admitted before release: {before_release:?}"
            );
            assert_eq!(
                before_release,
                vec!["heavy-one", "light"],
                "the light binary must backfill the lane no heavy one fits"
            );
            let admitted_weight: usize = before_release
                .iter()
                .map(|name| weight(&make(name), &heavy).min(3))
                .sum();
            assert!(admitted_weight <= 3, "admitted weight {admitted_weight}");
            let outcome = outcome.unwrap().unwrap();
            assert_eq!(outcome.failed.len(), 0, "{outcome:?}");
            assert_eq!(outcome.ran, 4, "{outcome:?}");
        });
        let log = fs::read_to_string(settings.log).unwrap();
        assert!(
            log.contains("runner: xtask/bin/heavy-one assigned 2/3 lanes and 1/3 test threads"),
            "{log}"
        );
        assert!(
            log.contains("runner: xtask/bin/light assigned 1/3 lanes and 1/3 test threads"),
            "{log}"
        );
        assert!(
            log.contains("runner: 4 of 4 binaries ran on 3 lanes"),
            "{log}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schedule_runs_unmeasured_then_longest_first() {
        let binaries = vec![
            binary("a", "lib", "short"),
            binary("b", "lib", "long"),
            binary("c", "lib", "new"),
        ];
        let mut durations = BTreeMap::new();
        durations.insert(binaries[0].id(), 100);
        durations.insert(binaries[1].id(), 90_000);
        let order: Vec<_> = schedule(&binaries, &durations)
            .into_iter()
            .map(|b| b.target_name)
            .collect();
        assert_eq!(order, vec!["new", "long", "short"]);
    }

    /// Real binaries (this test executable, run with a filter that selects
    /// one known test) through the runner: blocks land in the log under
    /// Cargo's header, durations are recorded, and a failing binary stops
    /// the queue.
    #[test]
    fn runner_appends_blocks_and_stops_after_a_failure() {
        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("susi-runner-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let make = |name: &str| Binary {
            package: "xtask".into(),
            version: "0.1.0".into(),
            manifest_path: manifest.clone(),
            target_name: name.into(),
            kind: "bin".into(),
            src_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/local-workflow.rs"),
            executable: exe.clone(),
        };
        let log = dir.join("gate.log");
        let durations = dir.join("durations.json");
        let settings = Settings {
            concurrency: 2,
            total_test_threads: 2,
            build_jobs: 2,
            home_root: &dir.join("homes"),
            target_dir: &dir.join("target"),
            triple: Some("x86_64-test"),
            durations: &durations,
            heavy: &[],
            log: &log,
        };
        // Each launch runs one named test of this very executable; the
        // second name does not exist, so that harness exits 0 with 0 tests,
        // and the third asks the harness for a test that fails: `--exact`
        // on an unknown name with `--include-ignored` still exits 0, so use a
        // bogus flag to make a binary fail to start cleanly.
        let launch = |binary: &Binary, home: &Path| -> Result<Command> {
            let mut command = Command::new(&binary.executable);
            command.env("HOME", home).env("USERPROFILE", home);
            match binary.target_name.as_str() {
                "passes" => {
                    command.args([
                        "--exact",
                        "test_runner::tests::lanes_and_threads_scale_with_the_gate_share",
                    ]);
                }
                "fails" => {
                    command.arg("--definitely-not-a-libtest-flag");
                }
                _ => {}
            }
            Ok(command)
        };
        let binaries = vec![make("passes"), make("fails")];
        let outcome = run(&binaries, &settings, &launch).unwrap();
        assert_eq!(outcome.ran, 2);
        assert_eq!(outcome.failed.len(), 1, "{outcome:?}");
        assert_eq!(outcome.failed[0].0, "xtask/bin/fails");
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("     Running unittests"), "{text}");
        assert!(text.contains("test result: ok. 1 passed"), "{text}");
        assert!(
            text.contains("runner: xtask/bin/fails exited with"),
            "{text}"
        );
        assert!(
            text.contains("runner: 2 of 2 binaries ran on 2 lanes"),
            "{text}"
        );
        let recorded: BTreeMap<String, u64> =
            serde_json::from_slice(&fs::read(&durations).unwrap()).unwrap();
        assert!(recorded.contains_key("xtask/bin/passes"));
        // A failure stops further binaries from starting. Fresh durations
        // keep the queue in the given order (unmeasured binaries keep their
        // order), so the failing binary runs first on the single lane.
        let fresh = dir.join("durations-2.json");
        let binaries = vec![make("fails"), make("passes"), make("passes")];
        let outcome = run(
            &binaries,
            &Settings {
                concurrency: 1,
                durations: &fresh,
                ..settings
            },
            &launch,
        )
        .unwrap();
        assert_eq!(outcome.ran, 1, "{outcome:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn runner_releases_weighted_lanes_after_spawn_failure() {
        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("susi-runner-spawn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let make = |name: &str| Binary {
            package: "xtask".into(),
            version: "0.1.0".into(),
            manifest_path: manifest.clone(),
            target_name: name.into(),
            kind: "bin".into(),
            src_path: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/local-workflow.rs"),
            executable: exe.clone(),
        };
        let missing = exe.with_file_name(format!("missing-spawn-{}.exe", std::process::id()));
        assert!(!missing.exists());
        let settings = Settings {
            concurrency: 6,
            total_test_threads: 3,
            build_jobs: 6,
            home_root: &dir.join("homes"),
            target_dir: &dir.join("target"),
            triple: None,
            durations: &dir.join("durations.json"),
            heavy: &[],
            log: &dir.join("gate.log"),
        };
        let launch = |binary: &Binary, _home: &Path| -> Result<Command> {
            let mut command = if binary.target_name == "local-workflow" {
                Command::new(&missing)
            } else {
                Command::new(&exe)
            };
            if binary.target_name != "local-workflow" {
                command.arg("--list");
            }
            Ok(command)
        };
        let binaries = vec![
            make("local-workflow"),
            make("ordinary-one"),
            make("ordinary-two"),
        ];
        let outcome = run(&binaries, &settings, &launch).unwrap();
        assert!(
            outcome
                .failed
                .iter()
                .any(|(id, code)| id == "xtask/bin/local-workflow" && code.is_none()),
            "{outcome:?}"
        );
        let log = fs::read_to_string(settings.log).unwrap();
        assert!(log.contains("could not start"), "{log}");
        assert!(
            log.contains(
                "runner: xtask/bin/local-workflow assigned 2/3 lanes and 2/3 test threads"
            ),
            "{log}"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
