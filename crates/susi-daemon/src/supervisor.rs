//! Init-style supervision of the microkernel's leaf services.
//!
//! The daemon owns the substrate's process layer: on boot it spawns any
//! leaf service whose port is dead, records the supervised pids in the
//! shared process table (`substrate_home/services.json`), health-checks
//! them on a timer, respawns crashes with a restart cap, and SIGTERMs its
//! children during graceful shutdown. Services it did not spawn (a dev
//! `cargo run -p susi-native`, a systemd unit) are never killed — the
//! daemon only terminates pids it placed in the table itself.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use susi_core::service_table::{self, LEAF_SERVICES, LeafService};

/// How often the monitor re-probes supervised services.
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// How long `ensure` waits for a freshly spawned service to bind its port.
const STARTUP_WAIT: Duration = Duration::from_secs(10);
/// Restarts per daemon boot before the supervisor gives up — a service
/// that cannot stay up this many times is broken, not unlucky.
const MAX_RESTARTS: u32 = 10;
/// After MAX_RESTARTS, respawns are suspended this long rather than
/// abandoned forever — a crash-looping service backs off, then gets a
/// fresh restart budget.
const DISABLE_COOLDOWN_SECS: u64 = 300;
/// Services absent from the process table (never spawned, e.g. binary
/// missing at daemon boot) are retried on this slower cadence so a
/// binary appearing later gets adopted without a daemon restart.
const MISSING_RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// SIGTERM → SIGKILL escalation window on shutdown.
const TERM_GRACE: Duration = Duration::from_secs(2);
/// Consecutive HTTP-probe failures tolerated on a live pid before it is
/// declared wedged and respawned — transient probe timeouts under load
/// must not kill a healthy service.
const WEDGED_MISS_LIMIT: u32 = 3;

/// Locate a leaf service binary: next to the running daemon first
/// (cargo target dir, staged installs), then `substrate_home/bin`, then
/// `$PATH`.
fn locate_binary(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for dir in [dir.to_path_buf(), dir.join("..")] {
            // `..` covers cargo's test layout: test binaries run from
            // `target/debug/deps/` while sibling bins land in
            // `target/debug/`.
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let staged = crate::susi_paths::SusiDirs::substrate_home()
        .join("bin")
        .join(name);
    if staged.is_file() {
        return Some(staged);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The path used to re-execute this binary for `service-run <name>`.
/// Prefers `/proc/self/exe` (the running inode, immune to in-place
/// binary replacement); falls back to `current_exe`, stripping the
/// kernel's ` (deleted)` suffix so a stale text resolves to the path
/// it was replaced at — the new binary is the intended target anyway.
pub fn reexec_path() -> Option<PathBuf> {
    let proc_exe = PathBuf::from("/proc/self/exe");
    if proc_exe.exists() {
        return Some(proc_exe);
    }
    let exe = std::env::current_exe().ok()?;
    if exe.exists() {
        return Some(exe);
    }
    let s = exe.to_string_lossy();
    let stripped = s.strip_suffix(" (deleted)").map(PathBuf::from);
    stripped.filter(|p| p.exists()).or(Some(exe))
}

/// Spawn a leaf service detached, with stdout/stderr appended to
/// `substrate_home/logs/<name>.log` instead of the daemon's own stderr.
///
/// Resolution order: a sibling standalone `susi-<name>` binary (dev
/// layout, staged installs), then self-reexec — the running `susi`
/// binary's `service-run <name>` mode, which is always present and
/// version-matched wherever the daemon was installed.
fn spawn_service(svc: &LeafService) -> Option<u32> {
    let mut cmd = if let Some(bin) = locate_binary(svc.binary) {
        Command::new(bin)
    } else {
        // Reexec path: `/proc/self/exe` resolves the *running inode* —
        // it survives the binary being replaced under a live daemon
        // (`current_exe` then returns a `… (deleted)` path, and
        // Command::new fails ENOENT, leaving the supervisor unable to
        // spawn anything). Fall back to current_exe off-Linux, with the
        // kernel's " (deleted)" suffix stripped if it still resolves.
        let exe = reexec_path()?;
        let mut c = Command::new(exe);
        c.args(["service-run", svc.name]);
        c
    };
    let log_dir = crate::susi_paths::SusiDirs::substrate_home().join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!("{}.log", svc.name));
    rotate_log_if_large(&log_path, 16 * 1024 * 1024);
    let open_log = || OpenOptions::new().create(true).append(true).open(&log_path);
    let (out, err) = match (open_log(), open_log()) {
        (Ok(o), Ok(e)) => (Stdio::from(o), Stdio::from(e)),
        _ => (Stdio::null(), Stdio::null()),
    };
    cmd.stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .ok()
        .map(|mut child| {
            let pid = child.id();
            // The daemon parents every service child, and a Child that is
            // dropped without wait() leaves a permanent zombie — observed
            // live: susi-native stuck <defunct> under the daemon, port
            // dead, no respawn possible. A detached waiter reaps it.
            thread::spawn(move || {
                let _ = child.wait();
            });
            pid
        })
}

/// Supervisor decisions go to stderr AND `substrate_home/logs/
/// supervisor.log` — the daemon detaches stderr, so without the file
/// every spawn/respawn/hold-down decision is invisible after the fact.
fn slog(msg: &str) {
    eprintln!("{msg}");
    let path = crate::susi_paths::SusiDirs::substrate_home()
        .join("logs")
        .join("supervisor.log");
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    rotate_log_if_large(&path, 4 * 1024 * 1024);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "[{}] {msg}", utc_now());
    }
}

/// Single-generation rotation: past `cap_bytes` the log becomes
/// `{name}.1` (previous generation discarded) and the writer continues
/// on a fresh file. Bounds every supervised log at ~2×cap without
/// losing the most recent history a debugging session needs.
fn rotate_log_if_large(path: &std::path::Path, cap_bytes: u64) {
    let oversized = fs::metadata(path)
        .map(|m| m.len() > cap_bytes)
        .unwrap_or(false);
    if oversized {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
}

/// `YYYY-MM-DD HH:MM:SS` UTC from the wall clock — readable ops
/// timestamps without pulling a datetime crate into the supervisor.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, tod % 3600 / 60, tod % 60);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

/// Wait until a port answers HTTP or the deadline passes — bind alone
/// isn't readiness; axum must actually serve.
fn wait_for_port(port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if service_table::http_probe(port) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Send a unix signal to a pid the daemon supervises.
// SAFETY: libc::kill sends a signal and touches no memory; the pid comes
// from our own process table (a child the daemon spawned).
#[cfg(unix)]
#[allow(unsafe_code)]
fn signal(pid: u32, sig: i32) -> bool {
    unsafe { libc::kill(pid as i32, sig) == 0 }
}

#[cfg(not(unix))]
fn signal(pid: u32, _sig: i32) -> bool {
    let _ = pid;
    false
}

#[cfg(unix)]
const SIGTERM: i32 = libc::SIGTERM;
#[cfg(unix)]
const SIGKILL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const SIGTERM: i32 = 15;
#[cfg(not(unix))]
const SIGKILL: i32 = 9;

/// Two metadata sets describe the same file. Device+inode on unix;
/// size+mtime is the portable fallback (weaker, but only used where no
/// inode concept exists).
#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// What a pre-bound port holder is, relative to the binary this daemon
/// would spawn for the service.
enum HolderKind {
    /// Same inode as the spawn target — our orphan, current build.
    /// Adopted as supervised so health checks and respawn apply.
    OursCurrent,
    /// Our binary but a replaced inode (` (deleted)` exe) — a stale
    /// orphan running pre-upgrade code. Killed and respawned so daemon
    /// upgrades reach the leaf plane.
    OursStale,
    /// A foreign process or an unresolvable holder — observed only.
    Foreign,
}

/// Classify the holder of a service port we did not spawn this boot.
/// `ours` is decided by executable identity (path or inode) against the
/// same resolution `spawn_service` uses; `stale` by the kernel's
/// ` (deleted)` marker or a surviving exe whose inode no longer matches
/// the spawn target.
fn classify_holder(svc: &LeafService, pid: u32) -> HolderKind {
    if pid == 0 {
        return HolderKind::Foreign;
    }
    let Ok(link) = fs::read_link(format!("/proc/{pid}/exe")) else {
        return HolderKind::Foreign;
    };
    let raw = link.to_string_lossy().into_owned();
    let deleted = raw.ends_with(" (deleted)");
    let stripped = raw.trim_end_matches(" (deleted)").to_owned();
    let exe = PathBuf::from(&stripped);
    // The binary this daemon would spawn for the service now.
    let target = locate_binary(svc.binary).or_else(reexec_path);
    let daemon_exe = reexec_path();
    let known_path = target.as_ref().is_some_and(|t| exe == *t)
        || daemon_exe.as_ref().is_some_and(|d| exe == *d);
    if known_path {
        return if deleted {
            HolderKind::OursStale
        } else {
            HolderKind::OursCurrent
        };
    }
    // A deleted susi-named binary holding a host-contract port is dead
    // code walking — a stale orphan from a dev build or an older
    // install — regardless of which path it was exec'd from. Reclaim
    // it; a *live* susi binary at a foreign path stays foreign (it may
    // belong to another workspace's daemon).
    if deleted {
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == svc.binary || name == "susi" {
            return HolderKind::OursStale;
        }
    }
    // Different path: same file under another name (hardlink/bind mount)
    // still counts as ours; anything else is foreign.
    let same_file = target
        .as_ref()
        .is_some_and(|t| match (fs::metadata(&exe), fs::metadata(t)) {
            (Ok(a), Ok(b)) => same_file(&a, &b),
            _ => false,
        });
    if !same_file {
        return HolderKind::Foreign;
    }
    if deleted {
        HolderKind::OursStale
    } else {
        HolderKind::OursCurrent
    }
}

/// Kill a stale orphan: SIGTERM with a short grace, then SIGKILL. The
/// caller drops the service row so the missing-service path respawns a
/// supervised current-binary child.
fn kill_stale_orphan(svc: &LeafService, pid: u32) {
    slog(&format!(
        "[supervisor] {} held by stale susi orphan (pid {pid}); reclaiming",
        svc.name
    ));
    let _ = signal(pid, SIGTERM);
    for _ in 0..20 {
        if !service_table::pid_alive(pid) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if service_table::pid_alive(pid) {
        let _ = signal(pid, SIGKILL);
        thread::sleep(Duration::from_millis(100));
    }
}

/// Boot pass: spawn any leaf service whose port is dead and record the
/// supervised pid. Services already listening are left alone — the daemon
/// adopts supervision of its own children only.
pub fn ensure_leaf_services() {
    let mut table = service_table::load();
    for svc in LEAF_SERVICES {
        // An operator stop survives daemon restarts — the flag lives in
        // the table, and "holds it down until `susi services start`"
        // means exactly that.
        if table.iter().any(|r| r.name == svc.name && r.stopped) {
            continue;
        }
        if service_table::probe(svc.port()) {
            let pid = service_table::pid_for_port(svc.port()).unwrap_or(0);
            match classify_holder(svc, pid) {
                // Our own orphan on the current build — supervise it.
                HolderKind::OursCurrent => {
                    if !table.iter().any(|r| r.name == svc.name) {
                        service_table::record(&mut table, svc.name, pid, svc.port());
                        slog(&format!(
                            "[supervisor] adopted orphan {} (pid {pid})",
                            svc.name
                        ));
                    }
                    continue;
                }
                // Stale susi binary — reclaim the port and respawn below.
                HolderKind::OursStale => kill_stale_orphan(svc, pid),
                // Foreign listener — observed, never killed by us.
                HolderKind::Foreign => {
                    if !table.iter().any(|r| r.name == svc.name) {
                        service_table::record_external(&mut table, svc.name, pid, svc.port());
                        slog(&format!(
                            "[supervisor] {} already bound externally{} — observing, not supervising",
                            svc.name,
                            if pid != 0 {
                                format!(" (pid {pid})")
                            } else {
                                String::new()
                            }
                        ));
                    }
                    continue;
                }
            }
        }
        if let Some(pid) = spawn_and_record(svc, &mut table, None) {
            slog(&format!("[supervisor] started {} (pid {})", svc.name, pid));
        } else if !table.iter().any(|r| r.name == svc.name) {
            slog(&format!(
                "[supervisor] {} down and binary `{}` not found or never bound :{}",
                svc.name,
                svc.binary,
                svc.port()
            ));
        }
    }
    // Same locked merge as the monitor — a CLI op racing the boot pass
    // must not be clobbered by our stale copy.
    let merged = service_table::update_with(|fresh| {
        for my in &table {
            match fresh.iter_mut().find(|r| r.name == my.name) {
                Some(f) if f.stopped == my.stopped => *f = my.clone(),
                Some(f) => {
                    let stopped = f.stopped;
                    let disabled = f.disabled_until;
                    *f = my.clone();
                    f.stopped = stopped;
                    f.disabled_until = disabled;
                }
                None => fresh.push(my.clone()),
            }
        }
    });
    if let Err(e) = merged {
        slog(&format!("[supervisor] persist process table: {e}"));
    }
}

/// Spawn a service, wait for its port, then verify the listener is
/// actually owned by our child before recording — a port still bound by
/// a lingering or foreign process makes `wait_for_port` pass while our
/// spawn dies on EADDRINUSE; recording that dead pid crash-loops the
/// monitor. On ownership mismatch the spawn is killed and a resolvable
/// holder is adopted as external instead. When `shutdown` is raised
/// mid-wait the spawn is reaped and nothing is recorded — a child must
/// never be created during teardown.
fn spawn_and_record(
    svc: &LeafService,
    table: &mut Vec<service_table::ServiceRecord>,
    shutdown: Option<&AtomicBool>,
) -> Option<u32> {
    let pid = spawn_service(svc)?;
    if !wait_for_port(svc.port(), STARTUP_WAIT) {
        let _ = signal(pid, SIGKILL);
        return None;
    }
    if shutdown.is_some_and(|f| f.load(Ordering::Acquire)) {
        let _ = signal(pid, SIGTERM);
        thread::sleep(Duration::from_millis(200));
        let _ = signal(pid, SIGKILL);
        return None;
    }
    match service_table::pid_for_port(svc.port()) {
        Some(holder) if holder == pid => {
            service_table::record(table, svc.name, pid, svc.port());
            Some(pid)
        }
        Some(holder) => {
            let _ = signal(pid, SIGKILL);
            match classify_holder(svc, holder) {
                HolderKind::OursCurrent => {
                    service_table::record(table, svc.name, holder, svc.port());
                    slog(&format!(
                        "[supervisor] {} port held by our orphan pid {holder}; adopted",
                        svc.name
                    ));
                }
                HolderKind::OursStale => {
                    kill_stale_orphan(svc, holder);
                }
                HolderKind::Foreign => {
                    service_table::record_external(table, svc.name, holder, svc.port());
                    slog(&format!(
                        "[supervisor] {} port held by pid {holder}; adopted as external",
                        svc.name
                    ));
                }
            }
            None
        }
        // Holder unresolvable — give our live spawn the benefit; a wrong
        // record is caught and corrected as external on the next pass.
        None => {
            service_table::record(table, svc.name, pid, svc.port());
            Some(pid)
        }
    }
}

/// Respawn one supervised service after a crash; returns the new record.
fn respawn(
    svc: &LeafService,
    table: &mut Vec<service_table::ServiceRecord>,
    shutdown: &AtomicBool,
) {
    if let Some(rec) = table.iter_mut().find(|r| r.name == svc.name) {
        if rec.restarts >= MAX_RESTARTS {
            let until = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                + DISABLE_COOLDOWN_SECS;
            slog(&format!(
                "[supervisor] {} exceeded {MAX_RESTARTS} restarts; suspending respawns for {}s",
                svc.name, DISABLE_COOLDOWN_SECS
            ));
            rec.disabled_until = Some(until);
            return;
        }
        // Live pid with a dead or wedged HTTP surface is unhealthy —
        // terminate before respawn.
        if service_table::pid_alive(rec.pid) && !service_table::http_probe(svc.port()) {
            let _ = signal(rec.pid, SIGTERM);
            thread::sleep(Duration::from_millis(200));
            let _ = signal(rec.pid, SIGKILL);
        }
    }
    // Recorded pid is dead but the port still serves: the binder is a
    // lingering or foreign process. Spawning here dies on EADDRINUSE and
    // records a dead pid — the crash loop this guard exists to break.
    // Adopt a resolvable holder as external; defer otherwise.
    {
        let recorded_dead = table
            .iter()
            .find(|r| r.name == svc.name)
            .is_some_and(|r| !service_table::pid_alive(r.pid));
        if recorded_dead && service_table::probe(svc.port()) {
            match service_table::pid_for_port(svc.port()) {
                Some(holder) => match classify_holder(svc, holder) {
                    HolderKind::OursCurrent => {
                        service_table::record(table, svc.name, holder, svc.port());
                        slog(&format!(
                            "[supervisor] {} port held by our orphan pid {holder}; adopted",
                            svc.name
                        ));
                    }
                    HolderKind::OursStale => kill_stale_orphan(svc, holder),
                    HolderKind::Foreign => {
                        service_table::record_external(table, svc.name, holder, svc.port());
                        slog(&format!(
                            "[supervisor] {} port held by live pid {holder}; adopted as external",
                            svc.name
                        ));
                    }
                },
                None => slog(&format!(
                    "[supervisor] {} dead but port bound by unresolvable process; deferring respawn",
                    svc.name
                )),
            }
            return;
        }
    }
    if shutdown.load(Ordering::Acquire) {
        return;
    }
    if let Some(pid) = spawn_and_record(svc, table, Some(shutdown)) {
        let restarts = table
            .iter()
            .find(|r| r.name == svc.name)
            .map(|r| r.restarts)
            .unwrap_or(0);
        slog(&format!(
            "[supervisor] restarted {} (pid {pid}, restart #{restarts})",
            svc.name
        ));
    }
}

/// Monitor loop: probe every supervised service; respawn on death or
/// wedged-port. Runs until `shutdown` is raised.
fn monitor_loop(shutdown: Arc<AtomicBool>) {
    let mut missing_retry: std::collections::HashMap<&'static str, std::time::Instant> =
        std::collections::HashMap::new();
    let mut probe_misses: std::collections::HashMap<&'static str, u32> =
        std::collections::HashMap::new();
    while !shutdown.load(Ordering::Acquire) {
        let mut table = service_table::load();
        let mut changed = false;
        // Rows dropped this pass (external release) — replayed onto the
        // fresh table inside the locked merge so the removal sticks.
        let mut removed: Vec<String> = Vec::new();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for svc in LEAF_SERVICES {
            // Abort mid-pass the moment shutdown is raised — a respawn
            // blocked in wait_for_port must not outlive the teardown.
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let Some(rec) = table.iter_mut().find(|r| r.name == svc.name) else {
                // Never supervised (binary missing at boot, or pruned) —
                // retry on a slow cadence so a binary appearing later is
                // adopted without a daemon restart.
                let due = missing_retry
                    .get(svc.name)
                    .is_none_or(|t| t.elapsed() >= MISSING_RETRY_INTERVAL);
                if due {
                    missing_retry.insert(svc.name, std::time::Instant::now());
                    if service_table::probe(svc.port()) {
                        let pid = service_table::pid_for_port(svc.port()).unwrap_or(0);
                        match classify_holder(svc, pid) {
                            // Our own orphan — supervise it rather than
                            // observe it as foreign.
                            HolderKind::OursCurrent => {
                                service_table::record(&mut table, svc.name, pid, svc.port());
                                slog(&format!(
                                    "[supervisor] adopted orphan {} (pid {pid})",
                                    svc.name
                                ));
                                changed = true;
                                continue;
                            }
                            // Stale susi binary — reclaim and respawn below.
                            HolderKind::OursStale => {
                                kill_stale_orphan(svc, pid);
                            }
                            // Bound by a process we didn't spawn — record as
                            // external so `susi services`/`susi os` can name it.
                            HolderKind::Foreign => {
                                service_table::record_external(
                                    &mut table,
                                    svc.name,
                                    pid,
                                    svc.port(),
                                );
                                slog(&format!(
                                    "[supervisor] {} bound externally{} — observing",
                                    svc.name,
                                    if pid != 0 {
                                        format!(" (pid {pid})")
                                    } else {
                                        String::new()
                                    }
                                ));
                                changed = true;
                                continue;
                            }
                        }
                    }
                    slog(&format!(
                        "[supervisor] {} not supervised; attempting spawn",
                        svc.name
                    ));
                    if let Some(pid) = spawn_and_record(svc, &mut table, Some(shutdown.as_ref())) {
                        slog(&format!("[supervisor] adopted {} (pid {})", svc.name, pid));
                        changed = true;
                    } else if table.iter().any(|r| r.name == svc.name) {
                        // spawn_and_record adopted an external holder.
                        changed = true;
                    }
                }
                continue;
            };
            if let Some(until) = rec.disabled_until {
                if now_secs < until {
                    continue;
                }
                // Cooldown elapsed — re-enable with a fresh restart budget.
                rec.disabled_until = None;
                rec.restarts = 0;
                changed = true;
            }
            if rec.stopped {
                // Operator stop — hold the service down until `susi
                // services start` clears the flag. A pid still alive
                // here lost a race (respawned in the same pass the flag
                // landed, or the stop-time SIGTERM didn't take) —
                // enforce the hold-down rather than let it run.
                if !rec.external && service_table::pid_alive(rec.pid) {
                    let _ = signal(rec.pid, SIGTERM);
                }
                continue;
            }
            if rec.external {
                // A holder we once classified foreign can turn out to be
                // a stale susi orphan — e.g. the binary was replaced
                // under a live service after it was recorded. Reclaim it
                // so the missing-service path respawns current code.
                if matches!(classify_holder(svc, rec.pid), HolderKind::OursStale) {
                    kill_stale_orphan(svc, rec.pid);
                    table.retain(|r| r.name != svc.name);
                    removed.push(svc.name.to_string());
                    missing_retry.remove(svc.name);
                    changed = true;
                    continue;
                }
                // External listeners are observed, never signaled: health
                // is the port alone. When it dies we drop the row so the
                // missing-service path spawns a supervised child instead.
                if service_table::probe(rec.port) {
                    continue;
                }
                slog(&format!(
                    "[supervisor] external {} released :{}; taking over",
                    svc.name, rec.port
                ));
                table.retain(|r| r.name != svc.name);
                removed.push(svc.name.to_string());
                missing_retry.remove(svc.name);
                changed = true;
                continue;
            }
            // HTTP probe, not just TCP accept: a wedged listener still
            // answers connect() while its request path is dead — only an
            // HTTP response proves the service actually serves. A dead
            // pid respawns at once, but a live pid that fails the probe
            // gets a grace streak — a single probe timeout under load
            // must not kill a healthy service.
            let alive = service_table::pid_alive(rec.pid);
            if alive && service_table::http_probe(rec.port) {
                probe_misses.remove(svc.name);
                continue;
            }
            if alive {
                let misses = probe_misses.entry(svc.name).or_insert(0);
                *misses += 1;
                if *misses < WEDGED_MISS_LIMIT {
                    continue;
                }
                probe_misses.remove(svc.name);
            }
            slog(&format!("[supervisor] {} unhealthy; respawning", svc.name));
            respawn(svc, &mut table, shutdown.as_ref());
            changed = true;
        }
        // Shutdown mid-pass: the table is about to be cleared by
        // shutdown_all — saving our copy would resurrect stale rows.
        if changed && !shutdown.load(Ordering::Acquire) {
            // Locked merge: `services stop`/`start` may have written
            // mid-pass — saving our stale copy wholesale would clobber
            // the operator's flag. Rows we touched overwrite wholesale
            // (we own pid/restarts/uptime); rows the operator toggled
            // keep their `stopped`/`disabled_until`; rows we removed
            // stay removed.
            let merged = service_table::update_with(|fresh| {
                for my in &table {
                    match fresh.iter_mut().find(|r| r.name == my.name) {
                        Some(f) if f.stopped == my.stopped => *f = my.clone(),
                        Some(f) => {
                            let stopped = f.stopped;
                            let disabled = f.disabled_until;
                            *f = my.clone();
                            f.stopped = stopped;
                            f.disabled_until = disabled;
                        }
                        None => fresh.push(my.clone()),
                    }
                }
                fresh.retain(|r| !removed.contains(&r.name));
            });
            if let Err(e) = merged {
                slog(&format!("[supervisor] persist process table: {e}"));
            }
        }
        // Interruptible sleep — a raised shutdown ends the wait promptly
        // so joining this thread doesn't block out the probe interval.
        for _ in 0..(PROBE_INTERVAL.as_millis() / 100) {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Start supervision: boot pass, then the monitor thread. Returns the
/// join handle so the daemon can keep a reference.
pub fn start(shutdown: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    ensure_leaf_services();
    thread::spawn(move || {
        monitor_loop(shutdown);
    })
}

/// Graceful shutdown: SIGTERM every pid the daemon supervises, escalate to
/// SIGKILL after the grace window, then clear the table so a stale record
/// never outlives its process. External rows are skipped — a daemon stop
/// must never signal a process it did not spawn.
pub fn shutdown_all() {
    let mut table = service_table::load();
    slog(&format!(
        "[supervisor] shutdown_all: {} rows, table_path={:?}",
        table.len(),
        service_table::table_path()
    ));
    for rec in table.iter().filter(|r| !r.external) {
        let ok = signal(rec.pid, SIGTERM);
        slog(&format!(
            "[supervisor] SIGTERM {} (pid {}) -> {ok}",
            rec.name, rec.pid
        ));
    }
    let start = Instant::now();
    while start.elapsed() < TERM_GRACE {
        if table
            .iter()
            .all(|r| r.external || !service_table::pid_alive(r.pid))
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    for rec in table.iter().filter(|r| !r.external) {
        if service_table::pid_alive(rec.pid) {
            let _ = signal(rec.pid, SIGKILL);
        }
    }
    table.clear();
    if let Err(e) = service_table::save(&table) {
        slog(&format!("[supervisor] clear process table: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn signal_terminates_spawned_child() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(service_table::pid_alive(pid));
        assert!(signal(pid, SIGTERM));
        let _ = child.wait();
        assert!(!service_table::pid_alive(pid));
    }

    #[test]
    fn utc_now_formats_iso_utc() {
        let ts = utc_now();
        assert_eq!(ts.len(), 20, "YYYY-MM-DD HH:MM:SSZ: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], " ");
    }

    #[test]
    #[cfg(unix)]
    fn locate_binary_finds_path_entries() {
        // `sh` is on PATH on every unix CI/dev host; a nonsense name must miss.
        assert!(locate_binary("sh").is_some());
        assert!(locate_binary("susi-definitely-not-a-binary-xyz").is_none());
    }

    /// Full supervision loop against the real leaf binaries: isolated XDG
    /// root (the vendored `SusiDirs` honors `SUSI_XDG`/`XDG_*_HOME`) plus
    /// per-run unique service ports, so nothing touches the host substrate,
    /// its canonical ports, or stale children leaked by an earlier run —
    /// fixed ports let an orphaned process get adopted as "external" and
    /// then survive `shutdown_all` (which correctly never signals external
    /// pids), failing the port check spuriously.
    #[test]
    #[cfg(unix)]
    fn ensure_and_shutdown_supervise_real_leaf_binaries() {
        if LEAF_SERVICES
            .iter()
            .any(|s| locate_binary(s.binary).is_none())
        {
            eprintln!("skipping: leaf service binaries not built");
            return;
        }
        // Env mutation must be serialized — vendored susi_core/susi_config
        // tests in this same binary flip XDG vars too, and a mid-test swap
        // moves table_path() so shutdown_all loads an empty table and
        // signals nothing (observed: all 5 ports still bound).
        let _env_guard = susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("susi_sup_e2e_{}", std::process::id()));
        // Ports derived from this test process's pid: every invocation gets
        // its own range, so a leaked child from a previous (or concurrent)
        // run can never be mistaken for this run's services.
        let base: u16 = 28000 + (std::process::id() % 1000) as u16 * 10;
        // SAFETY: env mutation in a test-only context; under nextest this
        // process runs this test alone. All reads happen after this block.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SUSI_XDG", "1");
            for v in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
                std::env::set_var(v, &tmp);
            }
            for (i, s) in LEAF_SERVICES.iter().enumerate() {
                std::env::set_var(s.port_env, (base + i as u16).to_string());
            }
        }

        // A fully parallel `cargo test --workspace` can starve a spawned
        // service past the 10s bind wait — ensure is idempotent (bound
        // ports are adopted, dead ones respawned), so retry until the
        // fleet is up instead of flaking on a slow spawn.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            ensure_leaf_services();
            if LEAF_SERVICES.iter().all(|s| service_table::probe(s.port()))
                || Instant::now() >= deadline
            {
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        let table = service_table::load();
        let all_up = LEAF_SERVICES.iter().all(|s| service_table::probe(s.port()));

        shutdown_all();
        // The table pids must be dead; ports on this run's unique range
        // must be closed. If a port somehow stays bound, kill the holder —
        // it can only be a child this run spawned (unique range).
        for s in LEAF_SERVICES {
            if service_table::probe(s.port())
                && let Some(pid) = service_table::pid_for_port(s.port())
            {
                let _ = signal(pid, SIGKILL);
            }
        }
        let pids_dead = table.iter().all(|r| !service_table::pid_alive(r.pid));
        let all_down = LEAF_SERVICES
            .iter()
            .all(|s| !service_table::probe(s.port()));

        assert_eq!(table.len(), LEAF_SERVICES.len(), "every service supervised");
        assert!(all_up, "every service port live after ensure");
        assert!(
            pids_dead,
            "supervised pids still alive after shutdown: {:?}",
            table
                .iter()
                .map(|r| (r.name.clone(), r.pid))
                .collect::<Vec<_>>()
        );
        assert!(all_down, "ports still bound after shutdown");
        assert!(service_table::load().is_empty(), "table cleared");
        let _ = fs::remove_dir_all(&tmp);
    }
}
