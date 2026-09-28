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
use tracing::info;

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
        std::thread::spawn(move || {
            let elastic_scheduler = ElasticScheduler::default();

            // Mandate: Perform immediate Readiness Pulse on substrate boot
            let _ = Self::perform_substrate_audit(&home);
            let _ = Self::perform_host_readiness(&home);

            let mut last_pulse = std::time::Instant::now();
            let mut tuned_at_completed = 0usize;
            loop {
                // 1. Hardware Load Watchdog (High-Resolution)
                Self::perform_hardware_watchdog_audit(&home, &blackboard, &elastic_scheduler);
                // 1b. SLA-driven auto-tune over the live task table.
                Self::perform_auto_tune(&blackboard, &elastic_scheduler, &mut tuned_at_completed);

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
        });
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
