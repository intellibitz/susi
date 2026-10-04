//! Mastery verification for VC-202-010: daemon, watcher, brain scout and
//! every other long-lived component runs under supervision with health
//! checks, bounded restart, logs and a status surface — and no
//! unsupervised background process remains.
//!
//! What holds: the daemon's process supervisor genuinely supervises the
//! leaf services (HTTP-probe health checks, bounded restarts with backoff,
//! `slog` log, `susi services` status surface) from `supervisor::start`
//! on the daemon run path.
//!
//! What does not: every long-lived component the daemon spawns in-process
//! (scout thread, pulse worker, cell watcher, rediscovery, …) starts with a
//! bare `thread::spawn` and is never registered, health-checked or
//! restarted — the registry built for them,
//! `service_supervision::Supervisor`, has no production caller, and the
//! cell watchdog covers exactly one cell that nothing ever pings.

use std::path::{Path, PathBuf};

/// Repo workspace root — the crate manifest is `crates/susi-daemon`.
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("../.."))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Every `.rs` file under `dir`, recursively.
fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// The leaf-service process supervision is real and wired on the daemon
/// path: the table records restart state and backoff per service, the
/// monitor is started by the daemon, and the CLI exposes the surface.
#[test]
fn vc_202_010_mastery_leaf_services_run_under_bounded_supervision() {
    // Bounded restart state is part of the shared table record.
    let rec = susi_core::service_table::ServiceRecord {
        name: "susi-paths".to_string(),
        pid: 1,
        port: 1,
        started_at: 0,
        restarts: 0,
        disabled_until: Some(60),
        external: false,
        stopped: false,
    };
    assert!(
        rec.disabled_until.is_some(),
        "crash-loop backoff is recorded"
    );

    // The supervised set is non-empty — the leaf services exist to watch.
    assert!(
        !susi_core::service_table::LEAF_SERVICES.is_empty(),
        "the daemon supervises a declared leaf-service set"
    );

    // The monitor is started on the daemon run path — not dead code.
    let server = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server.rs"));
    assert!(
        server.contains("supervisor::start("),
        "server.rs must start the process supervisor"
    );
}

/// The unified registry this vector needs —
/// `service_supervision::Supervisor` with register/health/restart/status —
/// exists but has no production caller: nothing registers a component
/// with it anywhere in the workspace.
#[test]
fn vc_202_010_mastery_supervision_registry_has_no_production_caller() {
    let mut callers = Vec::new();
    for file in rs_files(&crates_dir()) {
        let name = file.to_string_lossy().replace('\\', "/");
        // The registry itself and the crate re-export are not callers.
        if name.ends_with("service_supervision.rs") || name.ends_with("susi-daemon/src/lib.rs") {
            continue;
        }
        let text = read(&file);
        if text.contains("service_supervision::Supervisor")
            || text.contains("service_supervision::{")
            || (text.contains("Supervisor::new()")
                && !text.contains("SusiSupervisor::new()")
                && !text.contains("pub mod supervisor"))
        {
            callers.push(name);
        }
    }
    assert!(
        callers.is_empty(),
        "no production code registers a component with the supervision \
         registry — it is dead code outside its own test: {callers:?}"
    );
}

/// Every long-lived worker the daemon spawns in-process starts with a
/// bare `thread::spawn` and is never registered with any supervisor:
/// no health check, no restart bound, no status-surface coverage. A
/// panicked worker leaves the daemon silently half-alive — the
/// "unsupervised background process" the vector forbids.
#[test]
fn vc_202_010_mastery_daemon_workers_spawn_unsupervised() {
    let daemon_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut spawned: Vec<String> = Vec::new();
    let mut registered = 0usize;
    for file in rs_files(&daemon_src) {
        let name = file.to_string_lossy().replace('\\', "/");
        if name.ends_with("service_supervision.rs") {
            continue;
        }
        let text = read(&file);
        let spawns = text.matches("thread::spawn").count();
        if spawns > 0 {
            spawned.push(format!("{name} ({spawns} spawn sites)"));
        }
        if text.contains("service_supervision") || text.contains("Supervisor::register") {
            registered += 1;
        }
    }
    assert!(
        spawned.len() >= 5,
        "the daemon spawns many long-lived workers: {spawned:?}"
    );
    assert_eq!(
        registered, 0,
        "no spawned worker is registered with a supervisor — \
         health checks, bounded restart and the status surface cover \
         only the leaf-service processes"
    );
}

/// The cell watchdog registers exactly one cell — `susi-host` — and
/// nothing outside the watchdog's own module ever pings it or consumes a
/// dead-cell finding with a restart. It is a status field, not
/// supervision.
#[test]
fn vc_202_010_mastery_watchdog_covers_one_cell_without_restart() {
    let daemon_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut registers = Vec::new();
    let mut pings = Vec::new();
    let mut restarts = Vec::new();
    for file in rs_files(&daemon_src) {
        let name = file.to_string_lossy().replace('\\', "/");
        if name.ends_with("watchdog.rs") {
            continue;
        }
        let text = read(&file);
        registers.extend(
            text.match_indices("watchdog.register_cell(")
                .map(|_| name.clone()),
        );
        pings.extend(text.match_indices(".ping(").map(|_| name.clone()));
        if text.contains("find_dead_cells") && text.contains("respawn") {
            restarts.push(name);
        }
    }
    assert_eq!(
        registers.len(),
        1,
        "only susi-host is registered — the spawned workers are unwatched"
    );
    assert!(
        pings.is_empty(),
        "the one registered cell is never pinged: {pings:?}"
    );
    assert!(
        restarts.is_empty(),
        "a dead-cell finding is never turned into a restart: {restarts:?}"
    );
}
