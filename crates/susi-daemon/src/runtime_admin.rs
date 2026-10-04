// SUSI Runtime Admin: Autonomous Substrate Administration & Drift Correction
// Reality Check Always On - Hardware-Aware Self-Tuning and Autonomous Experience Distillation

use crate::blackboard::SwarmBlackboard;
use crate::elastic_scheduler::ElasticScheduler;
use crate::susi_abi::swarm::{PheromoneKind, SwarmPheromone};
use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::SusiAuditLogger;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use susi_core::telemetry::TelemetrySnapshot;
use tracing::{info, warn};

const CPU_LOAD_THRESHOLD: f32 = 8.0;
const THERMAL_THRESHOLD_C: f32 = 85.0;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct SusiRuntimeAdmin;

impl SusiRuntimeAdmin {
    /// Bootstraps the administrative substrate background cycle.
    /// `substrate_home` is the host substrate root (`~/.susi`), never a project cwd.
    pub fn start_administration_cycle(substrate_home: &Path, blackboard: Arc<SwarmBlackboard>) {
        let home = substrate_home.to_path_buf();
        // Supervised like every other long-lived worker. The heartbeat
        // bound is an hour: one readiness pulse runs a full 23-agent
        // security-sweep mission and can legitimately take that long.
        crate::service_supervision::Supervisor::global().spawn(
            "runtime-admin",
            3,
            Some(Duration::from_secs(3600)),
            move || {
                let home = home.clone();
                let blackboard = Arc::clone(&blackboard);
                Some(move || {
                    let elastic_scheduler = ElasticScheduler::default();
                    let mut maintenance = Maintenance::new(&home);

                    // Mandate: Perform immediate Readiness Pulse on substrate boot
                    let _ = Self::perform_substrate_audit(&home);
                    let _ = Self::perform_host_readiness(&home);

                    let mut last_pulse = std::time::Instant::now();
                    let mut tuned_at_completed = 0usize;
                    loop {
                        crate::service_supervision::heartbeat("runtime-admin");
                        // 1. Hardware Load Watchdog (High-Resolution)
                        Self::perform_hardware_watchdog_audit(
                            &home,
                            &blackboard,
                            &elastic_scheduler,
                        );
                        // 1b. SLA-driven auto-tune over the live task table.
                        Self::perform_auto_tune(
                            &blackboard,
                            &elastic_scheduler,
                            &mut tuned_at_completed,
                        );

                        // 1c. Periodic maintenance on its own adaptive cadence.
                        if std::time::Instant::now() >= maintenance.next_due {
                            maintenance.tick();
                        }

                        // 2. Periodic host readiness (hourly) — not project work.
                        // Each pulse runs a full 23-agent security-sweep mission;
                        // a 5-minute cadence burned inference failover and filled
                        // the journal with 12KB traces ~288x/day.
                        if last_pulse.elapsed() > Duration::from_secs(3600) {
                            let _ = Self::perform_substrate_audit(&home);
                            let _ = Self::perform_host_readiness(&home);
                            last_pulse = std::time::Instant::now();
                        }

                        std::thread::sleep(Duration::from_secs(10));
                    }
                })
            },
        );
    }

    /// Evaluates a telemetry snapshot against the watchdog's stress
    /// thresholds and, if any are tripped, builds the `Observation`
    /// pheromone the rest of the swarm can react to. `susi-daemon` can't
    /// call `susi_gawd::agents::GawdAgentFleet::throttle_concurrency`
    /// directly (crate leaf order forbids the edge); depositing onto the
    /// shared blackboard lets a `gawd` process subscribed to
    /// `hardware.stress` throttle itself instead.
    fn stress_pheromone_from_snapshot(snapshot: &TelemetrySnapshot) -> Option<SwarmPheromone> {
        let load_1m = snapshot.load_avg_1m.unwrap_or(0.0);
        let thermal_stress = snapshot
            .max_temp_c()
            .is_some_and(|t| t > THERMAL_THRESHOLD_C);
        let power_stress = snapshot.critical_battery();
        let load_stress = load_1m > CPU_LOAD_THRESHOLD;

        if !(thermal_stress || power_stress || load_stress) {
            return None;
        }

        Some(SwarmPheromone {
            id: format!("hw-stress-{}", now_secs()),
            topic: "hardware.stress".to_string(),
            emitter_id: "susi-runtime-admin".to_string(),
            kind: PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({
                "thermal_stress": thermal_stress,
                "power_stress": power_stress,
                "load_stress": load_stress,
                "load_avg_1m": load_1m,
                "max_temp_c": snapshot.max_temp_c(),
            }),
            ttl_ms: 60_000,
            deposited_at: now_secs(),
        })
    }

    /// Hardware Watchdog: samples real host telemetry, deposits a
    /// `hardware.stress` observation when it's under stress, and (Swarm OS
    /// Bullet 28) feeds that observation into `elastic_scheduler` so the
    /// resulting concurrency target is republished as its own
    /// `scheduler.target_concurrency` observation for downstream consumers.
    fn perform_hardware_watchdog_audit(
        substrate_home: &Path,
        blackboard: &SwarmBlackboard,
        elastic_scheduler: &ElasticScheduler,
    ) {
        let snapshot = crate::telemetry::sample_and_record(Some(substrate_home));
        let Some(pheromone) = Self::stress_pheromone_from_snapshot(&snapshot) else {
            return;
        };
        blackboard.deposit_pheromone(pheromone.clone());

        let Some(target) = elastic_scheduler.apply_pheromone(&pheromone) else {
            return;
        };
        blackboard.deposit_pheromone(SwarmPheromone {
            id: format!("scheduler-target-{}", now_secs()),
            topic: "scheduler.target_concurrency".to_string(),
            emitter_id: "susi-elastic-scheduler".to_string(),
            kind: PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({ "target_concurrency": target }),
            ttl_ms: 60_000,
            deposited_at: now_secs(),
        });
    }

    /// SLA-driven concurrency tuning (auto_tune, Bullet 74) from the live
    /// task table: checks the SLA (depositing `sla.violation` on a miss),
    /// then applies one tune step to the same scheduler the stress signal
    /// drives — but only when new missions completed since the last step, so
    /// one stale sample cannot ratchet concurrency every cycle. The SLA's
    /// only target is the system's own bound on a mission, the execution
    /// lease; no success-rate target is invented.
    fn perform_auto_tune(
        blackboard: &SwarmBlackboard,
        elastic_scheduler: &ElasticScheduler,
        tuned_at_completed: &mut usize,
    ) {
        let tasks = susi_core::task_manager::SwarmTaskManager::global().list_tasks();
        let snapshot = crate::swarm_metrics::snapshot_from_tasks(&tasks);
        if snapshot.missions_completed <= *tuned_at_completed {
            // Also re-arm after the bounded task table evicts old records.
            *tuned_at_completed = (*tuned_at_completed).min(snapshot.missions_completed);
            return;
        }
        *tuned_at_completed = snapshot.missions_completed;
        let lease_secs = susi_config::SusiConfig::load_global()
            .unwrap_or_default()
            .execution_lease_secs();
        let targets = crate::sla_monitor::SlaTargets {
            max_avg_resolution_ms: Some(lease_secs.saturating_mul(1000)),
            min_success_rate: None,
        };
        blackboard.check_sla_and_alert(&snapshot, &targets);
        let before = elastic_scheduler.target_concurrency();
        let advice = crate::auto_tune::advise(&snapshot, &targets);
        let after = crate::auto_tune::apply_tune(elastic_scheduler, &snapshot, &targets);
        if after != before {
            blackboard.deposit_pheromone(SwarmPheromone {
                id: format!("scheduler-autotune-{}", now_secs()),
                topic: "scheduler.target_concurrency".to_string(),
                emitter_id: "susi-auto-tune".to_string(),
                kind: PheromoneKind::Observation,
                intensity: 1.0,
                payload: serde_json::json!({
                    "target_concurrency": after,
                    "previous": before,
                    "reason": advice.reason,
                    "missions_completed": snapshot.missions_completed,
                    "missions_succeeded": snapshot.missions_succeeded,
                    "avg_time_to_resolution_ms": snapshot.avg_time_to_resolution_ms,
                }),
                ttl_ms: 60_000,
                deposited_at: now_secs(),
            });
        }
    }

    /// Best-effort local scan of `substrate_home` for exfiltration risk:
    /// known credential/key files whose permissions grant group or other
    /// any access. This is the substrate-safety scan `perform_host_readiness`
    /// promises — no LLM-driven analysis, but a real, bounded, permission-based
    /// check rather than an inert placeholder. A `*.key`/`*token*` file
    /// existing under `~/.susi` is expected (cluster/node identity); the
    /// risk is exposure via permissive bits, not mere existence.
    #[cfg(unix)]
    fn scan_for_exfiltration_risks(substrate_home: &Path) -> Vec<String> {
        use std::os::unix::fs::PermissionsExt;

        const SENSITIVE_NAME_FRAGMENTS: &[&str] = &[
            "key",
            "token",
            "secret",
            "credential",
            "id_rsa",
            "id_ed25519",
            ".pem",
        ];
        const MAX_ENTRIES: usize = 4096;

        let mut findings = Vec::new();
        let mut stack = vec![substrate_home.to_path_buf()];
        let mut visited = 0usize;

        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > MAX_ENTRIES {
                    return findings;
                }
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    stack.push(path);
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let lower = name.to_ascii_lowercase();
                if !SENSITIVE_NAME_FRAGMENTS
                    .iter()
                    .any(|frag| lower.contains(frag))
                {
                    continue;
                }
                if metadata.permissions().mode() & 0o077 != 0 {
                    findings.push(path.display().to_string());
                }
            }
        }
        findings
    }

    #[cfg(not(unix))]
    fn scan_for_exfiltration_risks(_substrate_home: &Path) -> Vec<String> {
        // Permission-based exposure checks are Unix-specific; nothing to
        // flag on platforms without POSIX mode bits.
        Vec::new()
    }

    /// Host-only readiness (models, substrate safety). Never treats
    /// `substrate_home` as a coding project — project work is always cwd.
    pub fn perform_host_readiness(substrate_home: &Path) -> EaiResult<()> {
        info!("[Readiness] Auditing model substrate optimal state...");
        let _ = susi_gemi::models::ModelManager::ensure_hardware_optimal_models(substrate_home);

        info!("[Readiness] Scanning substrate for exfiltration vectors...");
        let findings = Self::scan_for_exfiltration_risks(substrate_home);
        if !findings.is_empty() {
            // Detached daemon has no stdout — the audit chain is the
            // only durable surface an operator can inspect.
            SusiAuditLogger::log_event(
                substrate_home,
                "READINESS_VIOLATION",
                &format!(
                    "security pulse flagged {} exposed credential file(s)",
                    findings.len()
                ),
            );
            tracing::warn!(
                "[READINESS: SECURITY PROTOCOLS ENGAGED]\n{}",
                findings.join("\n")
            );
        }
        Ok(())
    }

    /// Hardware saturation audit, drift detection, and model substrate tuning.
    pub fn perform_substrate_audit(workspace: &Path) -> EaiResult<()> {
        let profile = susi_gemi::models::hardware::HardwareProfiler::get_profile();

        // 1. Hardware Saturation Audit
        if profile.acceleration_active {
            SusiAuditLogger::log_event(workspace, "SUBSTRATE_AUDIT", "GPU acceleration active.");
        } else if profile.ram_gb >= 16 {
            SusiAuditLogger::log_event(
                workspace,
                "SUBSTRATE_AUDIT",
                "System RAM sufficient for high-fidelity CPU inference.",
            );
        }

        // 2. Autonomous Drift Detection -- recorded, then acted on: a
        // recurring intent without a reflex gets one synthesized (rate-limited
        // to one attempt per intent per 24h inside the evolution manager).
        match susi_gawd::evolution::EvolutionManager::perform_autonomous_drift_audit(workspace) {
            Ok(report) => SusiAuditLogger::log_event(workspace, "DRIFT_AUDIT", &report),
            Err(e) => SusiAuditLogger::log_event(workspace, "DRIFT_AUDIT_FAILED", &e.to_string()),
        }
        let _ = susi_gawd::evolution::EvolutionManager::evolve_recurring_intent(workspace);

        // 3. Model Substrate Tuning
        let _ = susi_gemi::models::ModelManager::ensure_hardware_optimal_models(workspace);

        Ok(())
    }
}

/// Periodic daemon maintenance: scheduled missions, engine watchdog,
/// config hot-reload and the adaptive ecosystem probe. Every heavy step
/// is gated by [`crate::resource_governor`] so background work defers
/// while the host is under pressure.
struct Maintenance {
    watchdog: crate::engine_watchdog::EngineWatchdog,
    probes: crate::eco_probe_scheduler::ProbeScheduler,
    missions_path: std::path::PathBuf,
    dispatch_dir: std::path::PathBuf,
    drift_log: std::path::PathBuf,
    scout_journal: std::path::PathBuf,
    config_path: std::path::PathBuf,
    cfg_shadow: std::collections::BTreeMap<String, String>,
    cfg_mtime: Option<std::time::SystemTime>,
    stable_windows: u32,
    changes_last: u32,
    interval_secs: u64,
    next_due: std::time::Instant,
    last_report: Option<String>,
}

impl Maintenance {
    fn new(home: &Path) -> Self {
        let config_path = home.join("config.json");
        let cfg_shadow = Self::flat_config(&config_path);
        let cfg_mtime = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .ok();
        Self {
            watchdog: crate::engine_watchdog::EngineWatchdog::new(3, 500, 60_000),
            probes: crate::eco_probe_scheduler::ProbeScheduler::default(),
            missions_path: crate::scheduled_missions::ScheduleStore::path_in(home),
            dispatch_dir: home.join("missions"),
            drift_log: home.join("drift-alerts.jsonl"),
            scout_journal: home.join("brain_scout_runs.jsonl"),
            config_path,
            cfg_shadow,
            cfg_mtime,
            stable_windows: 0,
            changes_last: 1, // first probe always runs — the catalog starts unprobed
            interval_secs: 30,
            next_due: std::time::Instant::now(),
            last_report: None,
        }
    }

    /// Top-level scalar config fields as a flat map — the shadow a
    /// hot-reload diff is computed against.
    fn flat_config(path: &Path) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
                    .or_else(|| v.as_bool().map(|b| b.to_string()))
                    .or_else(|| v.as_f64().map(|f| f.to_string()))
                    .map(|s| (k, s))
            })
            .collect()
    }

    fn cpu_pct() -> f64 {
        let cores = std::fs::read_to_string("/proc/cpuinfo")
            .map(|t| t.lines().filter(|l| l.starts_with("processor")).count())
            .unwrap_or(1)
            .max(1);
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|t| t.split_whitespace().next().map(str::to_string))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        (load / cores as f64 * 100.0).clamp(0.0, 100.0)
    }

    fn mem_pct() -> f64 {
        let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
            return 0.0;
        };
        let kb = |key: &str| -> f64 {
            text.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        let total = kb("MemTotal");
        let avail = kb("MemAvailable");
        if total <= 0.0 {
            0.0
        } else {
            ((total - avail) / total * 100.0).clamp(0.0, 100.0)
        }
    }

    /// One maintenance pass: governor gate → config hot-reload → due
    /// missions → ecosystem probe (watchdog + drift) → adaptive cadence.
    fn tick(&mut self) {
        // Resource governor: background work defers under host pressure.
        let decision = crate::resource_governor::govern(Self::cpu_pct(), Self::mem_pct(), false);
        if !decision.allow {
            self.next_due = std::time::Instant::now() + Duration::from_secs(self.interval_secs);
            return;
        }
        let now = now_secs();

        // Config hot-reload: file edits apply in-process, no restart.
        let mtime = std::fs::metadata(&self.config_path)
            .and_then(|m| m.modified())
            .ok();
        if mtime.is_some() && mtime != self.cfg_mtime {
            let fresh = Self::flat_config(&self.config_path);
            let delta: std::collections::BTreeMap<String, String> = fresh
                .into_iter()
                .filter(|(k, v)| self.cfg_shadow.get(k) != Some(v))
                .collect();
            let events = crate::hot_reload_coverage::apply_hot_reload(&mut self.cfg_shadow, delta);
            for e in events.iter().filter(|e| e.applied) {
                info!("[runtime-admin] hot-reloaded config key {}", e.key);
            }
            self.cfg_mtime = mtime;
        }

        // Scheduled missions: due prompts dispatch to the missions
        // ingress dir and record their run (evidence + backoff).
        let mut store =
            crate::scheduled_missions::ScheduleStore::load(&self.missions_path).unwrap_or_default();
        let due: Vec<String> = store.due(now).iter().map(|m| m.id.clone()).collect();
        for id in due {
            let Some(mission) = store.missions.iter().find(|m| m.id == id).cloned() else {
                continue;
            };
            let _ = std::fs::create_dir_all(&self.dispatch_dir);
            let request = self.dispatch_dir.join(format!("{}-{now}.json", mission.id));
            let dispatched = serde_json::json!({
                "id": mission.id,
                "prompt": mission.prompt,
                "dispatched_unix": now,
            });
            let ok = std::fs::write(
                &request,
                serde_json::to_vec_pretty(&dispatched).unwrap_or_default(),
            )
            .is_ok();
            let finished = now_secs();
            if let Some(m) = store.missions.iter_mut().find(|m| m.id == id) {
                m.record_run(crate::scheduled_missions::RunRecord {
                    started_unix: now,
                    finished_unix: finished,
                    success: ok,
                    summary: if ok {
                        format!("dispatched to {}", request.display())
                    } else {
                        "dispatch failed".to_string()
                    },
                });
            }
            if !ok {
                warn!("[runtime-admin] scheduled mission {id} dispatch failed");
            }
        }
        if let Err(e) = store.save(&self.missions_path) {
            warn!("[runtime-admin] scheduled-missions save failed: {e}");
        }

        // Ecosystem probe on the adaptive rediscovery cadence: decide,
        // run, feed the engine watchdog and the drift detector.
        let subject = crate::eco_probe_scheduler::Subject {
            id: "local_ecosystem".to_string(),
            interval_secs: self.interval_secs,
            budget: 4,
            needs_consent: false, // local probe sends nothing off-host
        };
        let cond = crate::eco_probe_scheduler::Conditions {
            offline: std::env::var_os("SUSI_OFFLINE").is_some(),
            consented: true,
        };
        if let crate::eco_probe_scheduler::Decision::Run { .. } =
            self.probes.decide(&subject, now, cond)
        {
            let report = crate::discovery_pipeline::local_ecosystem_report();
            self.probes.record_run(&subject, now, 1);

            // Engine watchdog: crashed engines earn capped-backoff restarts.
            if let Some(engines) = report.get("engines").and_then(|e| e.as_array()) {
                for engine in engines {
                    let handle = crate::engine_watchdog::EngineHandle {
                        id: engine
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        healthy: engine
                            .get("running")
                            .and_then(|r| r.as_bool())
                            .unwrap_or(false),
                    };
                    match self.watchdog.on_health_check(&handle) {
                        crate::engine_watchdog::WatchdogAction::Restart { backoff_ms } => {
                            info!(
                                "[runtime-admin] engine {} unhealthy — restart in {}ms",
                                handle.id, backoff_ms
                            );
                        }
                        crate::engine_watchdog::WatchdogAction::MarkUnfit => {
                            warn!("[runtime-admin] engine {} marked unfit", handle.id);
                        }
                        crate::engine_watchdog::WatchdogAction::None => {}
                    }
                }
            }

            // Drift detector: the report's declared top-level fields vs
            // what actually came back — alerts become reviewable tasks.
            let declared: Vec<String> = ["kind", "hardware", "engines", "accelerators"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            if let Some(alert) = crate::eco_drift_alerts::detect_drift(
                "local_ecosystem",
                &declared,
                &report,
                &now.to_string(),
            ) {
                let task = crate::eco_drift_alerts::to_task(&alert);
                if let Ok(line) = serde_json::to_string(&task) {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.drift_log)
                    {
                        let _ = writeln!(f, "{line}");
                    }
                }
                self.changes_last = self.changes_last.saturating_add(alert.items.len() as u32);
            }
            let text = report.to_string();
            if self.last_report.as_deref() != Some(text.as_str()) {
                self.changes_last = self.changes_last.saturating_add(1);
                self.last_report = Some(text);
            }
        }

        // Brain scout on its own cadence (default 6h, SUSI_BRAIN_SCOUT_
        // INTERVAL_SECS): probes every registered provider through the real
        // registry and arbiter, records verified outcomes, snapshots the
        // leader per task class, and diffs against the journal's last run —
        // a leader change or a dead provider is reported drift, and each
        // run is appended so the previous ranking stays as evidence
        // (VC-202-003).
        let scout = crate::eco_probe_scheduler::Subject {
            id: "brain_scout".to_string(),
            interval_secs: susi_gemi::scout_schedule::interval_secs(),
            budget: 64,
            needs_consent: false, // only registered providers are probed;
                                  // local_only keeps clouds out of that registry already
        };
        if let crate::eco_probe_scheduler::Decision::Run { .. } =
            self.probes.decide(&scout, now, cond)
        {
            let (run, drift) = susi_gemi::scout_schedule::run_scheduled(now, &self.scout_journal);
            self.probes.record_run(&scout, now, run.providers.len());
            for d in &drift {
                warn!("[runtime-admin] brain scout drift: {d:?}");
                if let Ok(line) = serde_json::to_string(d) {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.drift_log)
                    {
                        let _ = writeln!(f, "{line}");
                    }
                }
            }
        }

        // Adaptive rediscovery: busy while the catalog changes, rare
        // once stable — drives both the tick cadence and probe interval.
        if self.changes_last == 0 {
            self.stable_windows = self.stable_windows.saturating_add(1);
        } else {
            self.stable_windows = 0;
        }
        let interval = crate::zc_rediscovery_adaptive::rediscovery_secs(
            self.changes_last,
            self.stable_windows,
        );
        self.interval_secs = interval.secs;
        self.changes_last = 0;
        self.next_due = std::time::Instant::now() + Duration::from_secs(self.interval_secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stress_yields_no_pheromone() {
        let snapshot = TelemetrySnapshot::empty();
        assert!(SusiRuntimeAdmin::stress_pheromone_from_snapshot(&snapshot).is_none());
    }

    #[test]
    fn high_load_deposits_an_observation_pheromone() {
        let snapshot = TelemetrySnapshot {
            thermal_zones: Vec::new(),
            batteries: Vec::new(),
            load_avg_1m: Some(99.0),
        };
        let pheromone = SusiRuntimeAdmin::stress_pheromone_from_snapshot(&snapshot).unwrap();
        assert_eq!(pheromone.topic, "hardware.stress");
        assert_eq!(pheromone.kind, PheromoneKind::Observation);
        assert_eq!(pheromone.payload["load_stress"], serde_json::json!(true));
    }

    #[test]
    #[cfg(unix)]
    fn readiness_scan_flags_world_readable_key_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("susi_readiness_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("cluster.key");
        std::fs::write(&key_path, b"secret").unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let findings = SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir);
        assert_eq!(findings.len(), 1);

        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
