//! Supervision for long-lived in-process components (VC-202-010).
//!
//! Every component that outlives a command — the HTTP servers, the pulse
//! worker, the pheromone evaporator, watchers, rediscovery and
//! administration loops — is spawned through this registry instead of a
//! bare `thread::spawn`. The daemon's main loop drives
//! [`Supervisor::supervise`]: a worker whose thread exited is restarted
//! within its bounded budget, a worker that stops heartbeating (or whose
//! watchdog cell goes silent) is marked hung and reported, every restart
//! is logged to the shared supervisor log, and one status surface
//! (`supervision.json` + `susi os`) reports all of it. A killed component
//! therefore recovers without a human noticing, and no unsupervised
//! background process remains.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Cadence the daemon drives one [`Supervisor::supervise`] round.
pub const SUPERVISE_INTERVAL: Duration = Duration::from_secs(5);

/// How long a finished one-shot stays on the status surface before the
/// registry prunes it — long enough to be observed, short enough that a
/// high-churn helper (cell spawners, reapers) cannot grow the map
/// without bound.
const DONE_VISIBLE_FOR: Duration = Duration::from_secs(300);

/// One restart, in order, for the supervision log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartEvent {
    pub name: String,
    pub attempt: u32,
}

/// What a supervised worker reports on the one status surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Thread alive, heartbeats fresh (when expected).
    Running,
    /// Thread alive but it stopped heartbeating or its watchdog cell
    /// went dead — detected and reported, but never respawned over the
    /// live copy (a second worker racing the wedged one is worse than
    /// an alert).
    Hung,
    /// Exited and the restart budget is exhausted, or it never spawned.
    Down,
    /// One-shot finished cleanly, or retired on purpose.
    Done,
}

impl WorkerState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerState::Running => "running",
            WorkerState::Hung => "hung",
            WorkerState::Down => "down",
            WorkerState::Done => "done",
        }
    }
}

/// What kind of workload a registered component is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    /// Runs for the daemon's life; any exit is a failure and restarts it
    /// (bounded by `max_restarts`).
    LongLived,
    /// Runs once; a clean return retires it, a panic restarts it
    /// (bounded by `max_restarts`).
    OneShot,
}

type Work = Box<dyn FnOnce() + Send>;
/// Builds the worker body once per (re)spawn so restart captures are
/// fresh. `None` means the body can no longer be built — the worker is
/// reported down, never silently dropped.
type Factory = Box<dyn FnMut() -> Option<Work> + Send>;

/// How a registered worker behaves across (re)spawns.
struct WorkerOpts {
    kind: WorkerKind,
    max_restarts: u32,
    heartbeat_timeout: Option<Duration>,
}

struct SupervisedService {
    name: String,
    kind: WorkerKind,
    healthy: bool,
    done: bool,
    max_restarts: u32,
    restarts: u32,
    factory: Option<Factory>,
    handle: Option<thread::JoinHandle<()>>,
    heartbeat_timeout: Option<Duration>,
    last_heartbeat: Instant,
    done_since: Option<Instant>,
}

/// The supervisor: one registry, one health pass, one status surface.
pub struct Supervisor {
    services: Mutex<BTreeMap<String, SupervisedService>>,
    log: Mutex<Vec<RestartEvent>>,
    watchdog: Mutex<Option<crate::watchdog::WatchdogManager>>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            services: Mutex::new(BTreeMap::new()),
            log: Mutex::new(Vec::new()),
            watchdog: Mutex::new(None),
        }
    }

    /// The process-wide registry every worker spawns through.
    pub fn global() -> &'static Supervisor {
        static GLOBAL: OnceLock<Supervisor> = OnceLock::new();
        GLOBAL.get_or_init(Supervisor::new)
    }

    /// Attach the swarm watchdog: every registered worker gets a
    /// watchdog cell, [`Supervisor::heartbeat`] pings it, and
    /// [`Supervisor::supervise`] treats a dead-cell finding as a hang.
    /// Workers registered before the attach are back-filled.
    pub fn attach_watchdog(&self, watchdog: crate::watchdog::WatchdogManager) {
        let names: Vec<String> = self
            .services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for name in &names {
            watchdog.register_cell(name);
        }
        *self.watchdog.lock().unwrap_or_else(|e| e.into_inner()) = Some(watchdog);
    }

    /// Spawn a long-lived worker under supervision. Any exit — return or
    /// panic — restarts it within `max_restarts`; a worker that stops
    /// heartbeating inside `heartbeat_timeout` (when `Some`) is marked
    /// hung.
    pub fn spawn<F, W>(
        &self,
        name: &str,
        max_restarts: u32,
        heartbeat_timeout: Option<Duration>,
        factory: F,
    ) where
        F: FnMut() -> Option<W> + Send + 'static,
        W: FnOnce() + Send + 'static,
    {
        self.register_worker(
            name,
            WorkerOpts {
                kind: WorkerKind::LongLived,
                max_restarts,
                heartbeat_timeout,
            },
            factory,
        );
    }

    /// Spawn a one-shot worker under supervision: registered and
    /// status-visible like everything else. A clean return retires it; a
    /// panic restarts it within `max_restarts`.
    pub fn spawn_oneshot<F, W>(&self, name: &str, max_restarts: u32, factory: F)
    where
        F: FnMut() -> Option<W> + Send + 'static,
        W: FnOnce() + Send + 'static,
    {
        self.register_worker(
            name,
            WorkerOpts {
                kind: WorkerKind::OneShot,
                max_restarts,
                heartbeat_timeout: None,
            },
            factory,
        );
    }

    fn register_worker<F, W>(&self, name: &str, opts: WorkerOpts, factory: F)
    where
        F: FnMut() -> Option<W> + Send + 'static,
        W: FnOnce() + Send + 'static,
    {
        let boxed: Factory = {
            let mut factory = factory;
            Box::new(move || factory().map(|w| Box::new(w) as Work))
        };
        let unique = {
            let mut map = self.services.lock().unwrap_or_else(|e| e.into_inner());
            // A live registration already owns this name — an auto-named
            // variant keeps both workers supervised instead of clobbering
            // (or worse, never spawning) one of them.
            let mut unique = name.to_string();
            let mut instance = 2u64;
            while map.get(&unique).is_some_and(|s| !s.done) {
                unique = format!("{name}#{instance}");
                instance += 1;
            }
            let mut svc = SupervisedService {
                name: unique.clone(),
                kind: opts.kind,
                healthy: true,
                done: false,
                max_restarts: opts.max_restarts,
                restarts: 0,
                factory: Some(boxed),
                handle: None,
                heartbeat_timeout: opts.heartbeat_timeout,
                last_heartbeat: Instant::now(),
                done_since: None,
            };
            svc.handle = spawn_for(&mut svc);
            if svc.handle.is_none() {
                svc.healthy = false;
                svc.done = true;
                svc.done_since = Some(Instant::now());
                crate::supervisor::slog(&format!(
                    "[supervision] {unique} could not spawn — reported down"
                ));
            }
            map.insert(unique.clone(), svc);
            unique
        };
        if let Some(watchdog) = &*self.watchdog.lock().unwrap_or_else(|e| e.into_inner()) {
            watchdog.register_cell(&unique);
        }
    }

    /// Record a liveness heartbeat — long-lived workers call
    /// [`heartbeat`] once per loop pass so a wedged loop is detected
    /// rather than assumed dead only when its thread is gone.
    pub fn heartbeat(&self, name: &str) {
        if let Some(watchdog) = &*self.watchdog.lock().unwrap_or_else(|e| e.into_inner()) {
            let _ = watchdog.ping(name);
        }
        if let Some(svc) = self
            .services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(name)
        {
            svc.last_heartbeat = Instant::now();
        }
    }

    /// Retire a worker intentionally (an explicit stop is not a
    /// failure): no restart, and the surface reports it done.
    pub fn retire(&self, name: &str) {
        if let Some(svc) = self
            .services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(name)
        {
            svc.done = true;
            svc.done_since = Some(Instant::now());
        }
        if let Some(watchdog) = &*self.watchdog.lock().unwrap_or_else(|e| e.into_inner()) {
            watchdog.unregister_cell(name);
        }
    }

    /// Join a supervised worker's current thread — the daemon uses it to
    /// wait out the leaf-service monitor during graceful shutdown.
    pub fn join(&self, name: &str) -> Option<thread::Result<()>> {
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(name)?
            .handle
            .take()
            .map(thread::JoinHandle::join)
    }

    /// One supervision round — driven every [`SUPERVISE_INTERVAL`] by
    /// the daemon loop. Exited long-lived workers are restarted within
    /// their bound; exited one-shots retire (restarting only on panic);
    /// live workers whose heartbeat or watchdog cell went silent are
    /// marked hung. Every transition is logged.
    pub fn supervise(&self) {
        // The watchdog is consulted before the services lock is taken:
        // heartbeat() locks watchdog → services and nothing may take
        // them in the reverse order.
        let dead_cells: Vec<String> = self
            .watchdog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(crate::watchdog::WatchdogManager::find_dead_cells)
            .unwrap_or_default();
        let now = Instant::now();
        let mut restarts = Vec::new();
        {
            let mut map = self.services.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, svc| {
                !(matches!(svc.kind, WorkerKind::OneShot)
                    && svc.done
                    && svc
                        .done_since
                        .is_some_and(|t| now.duration_since(t) > DONE_VISIBLE_FOR))
            });
            for svc in map.values_mut() {
                if svc.done {
                    continue;
                }
                if svc.handle.as_ref().is_some_and(|h| h.is_finished()) {
                    let panicked = svc
                        .handle
                        .take()
                        .map(|h| h.join().is_err())
                        .unwrap_or(false);
                    let restartable = match svc.kind {
                        WorkerKind::LongLived => true,
                        WorkerKind::OneShot => panicked,
                    };
                    if restartable && svc.restarts < svc.max_restarts {
                        svc.restarts += 1;
                        svc.last_heartbeat = now;
                        let attempt = svc.restarts;
                        crate::supervisor::slog(&format!(
                            "[supervision] {} {} — restart {}/{}",
                            svc.name,
                            if panicked { "panicked" } else { "exited" },
                            attempt,
                            svc.max_restarts
                        ));
                        svc.handle = spawn_for(svc);
                        if svc.handle.is_none() {
                            svc.done = true;
                            svc.done_since = Some(now);
                            svc.healthy = false;
                            crate::supervisor::slog(&format!(
                                "[supervision] {} could not respawn — reported down",
                                svc.name
                            ));
                        } else {
                            svc.healthy = true;
                            restarts.push(RestartEvent {
                                name: svc.name.clone(),
                                attempt,
                            });
                        }
                    } else {
                        svc.done = true;
                        svc.done_since = Some(now);
                        // A clean one-shot retirement is "done"; a panic
                        // or a long-lived worker past its budget is "down".
                        svc.healthy = matches!(svc.kind, WorkerKind::OneShot) && !panicked;
                        if !svc.healthy {
                            crate::supervisor::slog(&format!(
                                "[supervision] {} down — {} (restart budget {} exhausted)",
                                svc.name,
                                if panicked { "panicked" } else { "exited" },
                                svc.max_restarts
                            ));
                        }
                    }
                    continue;
                }
                // The thread is alive — a wedged worker is caught by its
                // heartbeat or watchdog cell, never by thread state.
                let stale_heartbeat = svc
                    .heartbeat_timeout
                    .is_some_and(|t| now.duration_since(svc.last_heartbeat) > t);
                let dead_cell = dead_cells.iter().any(|c| c == &svc.name);
                let healthy = !(stale_heartbeat || dead_cell);
                if healthy != svc.healthy {
                    crate::supervisor::slog(&format!(
                        "[supervision] {} {}",
                        svc.name,
                        if healthy {
                            "recovered"
                        } else {
                            "hung — heartbeat/watchdog silent"
                        }
                    ));
                }
                svc.healthy = healthy;
            }
        }
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(restarts);
    }

    /// The one status surface: every component's liveness and restart
    /// count.
    #[must_use]
    pub fn status(&self) -> Vec<ServiceStatus> {
        self.services
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|svc| ServiceStatus {
                name: svc.name.clone(),
                healthy: svc.healthy,
                restarts: svc.restarts,
                max_restarts: svc.max_restarts,
                state: worker_state(svc),
            })
            .collect()
    }

    /// The supervision log: restarts in the order they happened.
    #[must_use]
    pub fn restart_log(&self) -> Vec<RestartEvent> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The supervision report — the JSON form of [`Supervisor::status`]
    /// plus the restart log, surfaced through `susi os` and persisted to
    /// `substrate_home/supervision.json`.
    #[must_use]
    pub fn report_json(&self) -> serde_json::Value {
        let workers: Vec<serde_json::Value> = self
            .status()
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "state": s.state.as_str(),
                    "healthy": s.healthy,
                    "restarts": s.restarts,
                    "max_restarts": s.max_restarts,
                })
            })
            .collect();
        let restarts: Vec<serde_json::Value> = self
            .restart_log()
            .iter()
            .map(|e| serde_json::json!({ "name": e.name, "attempt": e.attempt }))
            .collect();
        serde_json::json!({
            "generated_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "workers": workers,
            "restart_log": restarts,
        })
    }

    /// Write the report where `susi os` and operators read it.
    pub fn persist_report(&self) {
        if let Ok(text) = serde_json::to_string_pretty(&self.report_json()) {
            let _ = susi_config::atomic_write_bytes(&report_path(), text.as_bytes());
        }
    }
}

/// Where the daemon persists the supervision surface for other
/// processes (`susi os`, operators).
fn report_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::substrate_home().join("supervision.json")
}

/// The live supervision report for this process's registry.
#[must_use]
pub fn report() -> serde_json::Value {
    Supervisor::global().report_json()
}

/// The last persisted supervision report — readable by any process
/// (e.g. the `susi` CLI when the daemon owns the live registry).
#[must_use]
pub fn load_report() -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(report_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Record a liveness heartbeat for `name` on the process-wide
/// supervisor — worker loops call this once per pass.
pub fn heartbeat(name: &str) {
    Supervisor::global().heartbeat(name);
}

/// Build and launch a worker's thread through its factory. `None` means
/// the body could not be built or the OS refused the spawn — the worker
/// is reported down, never dropped from the registry.
fn spawn_for(svc: &mut SupervisedService) -> Option<thread::JoinHandle<()>> {
    let work = (svc.factory.as_mut()?)()?;
    thread::Builder::new()
        .name(svc.name.clone())
        .spawn(work)
        .ok()
}

fn worker_state(svc: &SupervisedService) -> WorkerState {
    if svc.done {
        if svc.healthy {
            WorkerState::Done
        } else {
            WorkerState::Down
        }
    } else if svc.healthy {
        WorkerState::Running
    } else if svc.handle.is_some() {
        WorkerState::Hung
    } else {
        WorkerState::Down
    }
}

/// One component's status, as reported by [`Supervisor::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub name: String,
    pub healthy: bool,
    pub restarts: u32,
    pub max_restarts: u32,
    pub state: WorkerState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Worker threads need real scheduling time — assertions poll rather
    /// than race them.
    fn wait_until(cond: impl Fn() -> bool) -> bool {
        for _ in 0..400 {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    impl Supervisor {
        fn alive(&self, name: &str) -> bool {
            self.services
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)
                .and_then(|s| s.handle.as_ref())
                .is_some_and(|h| !h.is_finished())
        }

        fn state_of(&self, name: &str) -> WorkerState {
            self.status()
                .into_iter()
                .find(|s| s.name == name)
                .map(|s| s.state)
                .unwrap_or(WorkerState::Down)
        }
    }

    /// Acceptance: a supervised long-lived worker that dies is restarted
    /// within its bound, every restart is logged, the status surface
    /// reports it, and an exhausted budget leaves it down — reported,
    /// never silently dropped.
    #[test]
    fn vc_202_010_mastery() {
        let sup = Supervisor::new();
        let runs = Arc::new(AtomicUsize::new(0));
        {
            let runs = Arc::clone(&runs);
            sup.spawn("worker", 2, None, move || {
                let runs = Arc::clone(&runs);
                Some(move || {
                    runs.fetch_add(1, Ordering::SeqCst);
                })
            });
        }
        for expected_restarts in [1u32, 2] {
            assert!(
                wait_until(|| !sup.alive("worker")),
                "worker run must exit before the round"
            );
            sup.supervise();
            assert!(
                sup.alive("worker"),
                "restart {expected_restarts} respawned it"
            );
            assert_eq!(
                sup.state_of("worker"),
                WorkerState::Running,
                "restart {expected_restarts} reports running"
            );
        }
        // Third death: the budget of two is spent — down, reported.
        assert!(wait_until(|| !sup.alive("worker")));
        sup.supervise();
        assert_eq!(sup.state_of("worker"), WorkerState::Down);
        let st = sup
            .status()
            .into_iter()
            .find(|s| s.name == "worker")
            .unwrap();
        assert!(!st.healthy);
        assert_eq!(st.restarts, 2);
        // Every restart is logged, in order, with the attempt number.
        assert_eq!(
            sup.restart_log(),
            vec![
                RestartEvent {
                    name: "worker".into(),
                    attempt: 1
                },
                RestartEvent {
                    name: "worker".into(),
                    attempt: 2
                },
            ]
        );
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    /// A hung worker is detected (stale heartbeat and dead watchdog
    /// cell), reported, and never respawned over its live thread; a
    /// heartbeat clears the verdict again.
    #[test]
    fn hung_worker_is_detected_and_recovers_on_heartbeat() {
        let sup = Supervisor::new();
        sup.attach_watchdog(crate::watchdog::WatchdogManager::new(
            Duration::from_millis(40),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = Arc::clone(&stop);
            sup.spawn("sleeper", 3, Some(Duration::from_millis(40)), move || {
                let stop = Arc::clone(&stop);
                Some(move || {
                    while !stop.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                })
            });
        }
        // The worker never heartbeats — both detectors must fire.
        assert!(wait_until(|| {
            sup.supervise();
            sup.state_of("sleeper") == WorkerState::Hung
        }));
        assert!(sup.alive("sleeper"), "a hung worker is never cloned");
        // A heartbeat restores the verdict.
        sup.heartbeat("sleeper");
        sup.supervise();
        assert_eq!(sup.state_of("sleeper"), WorkerState::Running);
        stop.store(true, Ordering::Release);
    }

    /// One-shots retire on a clean return and restart only on panic,
    /// within their bound.
    #[test]
    fn oneshots_retire_cleanly_and_panics_restart_bounded() {
        let sup = Supervisor::new();
        sup.spawn_oneshot("clean", 0, || Some(|| {}));
        assert!(wait_until(|| {
            sup.supervise();
            sup.state_of("clean") == WorkerState::Done
        }));

        let attempts = Arc::new(AtomicUsize::new(0));
        {
            let attempts = Arc::clone(&attempts);
            sup.spawn_oneshot("flaky", 1, move || {
                let attempts = Arc::clone(&attempts);
                Some(move || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    panic!("flaky worker");
                })
            });
        }
        assert!(wait_until(|| {
            sup.supervise();
            sup.state_of("flaky") == WorkerState::Down
        }));
        // Initial run + the single restart the budget allowed.
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// Two live workers sharing a name are both kept under supervision —
    /// the second is registered under an auto-derived name rather than
    /// dropped or clobbered.
    #[test]
    fn live_name_collisions_keep_both_workers_supervised() {
        let sup = Supervisor::new();
        let stop = Arc::new(AtomicBool::new(false));
        for _ in 0..2 {
            let stop = Arc::clone(&stop);
            sup.spawn_oneshot("helper", 0, move || {
                let stop = Arc::clone(&stop);
                Some(move || {
                    while !stop.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                })
            });
        }
        assert!(
            sup.status()
                .iter()
                .any(|s| s.name == "helper" || s.name == "helper#2"),
            "both registrations visible: {:?}",
            sup.status()
        );
        assert!(sup.status().iter().any(|s| s.name == "helper#2"));
        stop.store(true, Ordering::Release);
    }
}
