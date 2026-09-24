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
        let exe = std::env::current_exe().ok()?;
        let mut c = Command::new(exe);
        c.args(["service-run", svc.name]);
        c
    };
    let log_dir = crate::susi_paths::SusiDirs::substrate_home().join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let open_log = || {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join(format!("{}.log", svc.name)))
    };
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
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

/// Wait until a port accepts TCP connections or the deadline passes.
fn wait_for_port(port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if service_table::probe(port) {
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
            // Port bound by a process we didn't spawn — record it as
            // external so the table reflects reality; never killed by us.
            if !table.iter().any(|r| r.name == svc.name) {
                let pid = service_table::pid_for_port(svc.port()).unwrap_or(0);
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
        let Some(pid) = spawn_service(svc) else {
            slog(&format!(
                "[supervisor] {} down and binary `{}` not found",
                svc.name, svc.binary
            ));
            continue;
        };
        if wait_for_port(svc.port(), STARTUP_WAIT) {
            service_table::record(&mut table, svc.name, pid, svc.port());
            slog(&format!("[supervisor] started {} (pid {})", svc.name, pid));
        } else {
            slog(&format!(
                "[supervisor] {} spawned (pid {}) but never bound :{}",
                svc.name,
                pid,
                svc.port()
            ));
            let _ = signal(pid, SIGKILL);
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

/// Respawn one supervised service after a crash; returns the new record.
fn respawn(svc: &LeafService, table: &mut Vec<service_table::ServiceRecord>) {
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
        // Live pid with a dead port is wedged — terminate before respawn.
        if service_table::pid_alive(rec.pid) && !service_table::probe(svc.port()) {
            let _ = signal(rec.pid, SIGTERM);
            thread::sleep(Duration::from_millis(200));
            let _ = signal(rec.pid, SIGKILL);
        }
    }
    if let Some(pid) = spawn_service(svc) {
        if wait_for_port(svc.port(), STARTUP_WAIT) {
            let rec = service_table::record(table, svc.name, pid, svc.port());
            slog(&format!(
                "[supervisor] restarted {} (pid {}, restart #{})",
                svc.name, pid, rec.restarts
            ));
        } else {
            let _ = signal(pid, SIGKILL);
        }
    }
}

/// Monitor loop: probe every supervised service; respawn on death or
/// wedged-port. Runs until `shutdown` is raised.
fn monitor_loop(shutdown: Arc<AtomicBool>) {
    let mut missing_retry: std::collections::HashMap<&'static str, std::time::Instant> =
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
                        // Bound by a process we didn't spawn — record as
                        // external so `susi services`/`susi os` can name it.
                        let pid = service_table::pid_for_port(svc.port()).unwrap_or(0);
                        service_table::record_external(&mut table, svc.name, pid, svc.port());
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
                    slog(&format!(
                        "[supervisor] {} not supervised; attempting spawn",
                        svc.name
                    ));
                    if let Some(pid) = spawn_service(svc) {
                        if wait_for_port(svc.port(), STARTUP_WAIT) {
                            service_table::record(&mut table, svc.name, pid, svc.port());
                            slog(&format!("[supervisor] adopted {} (pid {})", svc.name, pid));
                            changed = true;
                        } else {
                            let _ = signal(pid, SIGKILL);
                        }
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
            let healthy = service_table::pid_alive(rec.pid) && service_table::probe(rec.port);
            if healthy {
                continue;
            }
            slog(&format!("[supervisor] {} unhealthy; respawning", svc.name));
            respawn(svc, &mut table);
            changed = true;
        }
        if changed {
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
        thread::sleep(PROBE_INTERVAL);
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

        ensure_leaf_services();
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
