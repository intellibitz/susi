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
/// CPU-utilization stress threshold, used only where the OS keeps no load
/// average (Windows): sustained near-saturation of every logical CPU.
const CPU_UTIL_THRESHOLD_PCT: f32 = 95.0;
const THERMAL_THRESHOLD_C: f32 = 85.0;

// Synthetic diagnostic goals must never compete with the user's first goal.
// The explicit admin intent-ladder command remains available independently.
fn background_diagnostics_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

fn background_diagnostics_requested() -> bool {
    background_diagnostics_enabled(std::env::var("SUSI_BACKGROUND_DIAGNOSTICS").ok().as_deref())
}

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

                    // Keep model provisioning and the credential-exposure scan.
                    // Evolution/audit work starts on the hourly cadence below,
                    // never alongside the first user goal.
                    let _ = Self::perform_host_readiness(&home);

                    // Bounded start-up slice of the graded user-intent
                    // ladder (VC-201-001): a few low-grade intents run
                    // hermetically — scratch SUSI_HOME, throwaway
                    // workspace, no live estate, no egress — and the
                    // scorecard lands under the susi home. The ladder
                    // resolves from that home too, never from a cwd.
                    if background_diagnostics_requested()
                        && let Ok(card) = susi_gawd::intent_ladder::startup_slice(&home, &home)
                    {
                        // A failed rung lands in the host outbox, and an
                        // agent adopts it as a repair task in its own
                        // checkout; the task closes when the intent passes
                        // again (VC-201-012).
                        if let Err(e) = susi_gawd::intent_ladder::record_gaps(&home, &card) {
                            warn!("[runtime-admin] intent ladder gap outbox: {e}");
                        }
                    }

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
        // Windows keeps no load average; its CPU stress signal is the
        // sampled utilization instead.
        let load_stress = load_1m > CPU_LOAD_THRESHOLD
            || (snapshot.load_avg_1m.is_none()
                && snapshot
                    .cpu_util_pct
                    .is_some_and(|pct| pct > CPU_UTIL_THRESHOLD_PCT));

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
                "cpu_util_pct": snapshot.cpu_util_pct,
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

    /// Plane-bus rendezvous metadata under `<home>/bus/<pid>/`: the `topics`,
    /// `prefixes` and `caps` directories hold one endpoint file per registered
    /// topic or capability, named after it (`capability.tool.cluster_rekey_commit`),
    /// so their names match the sensitive fragments while holding no secret. A
    /// first goal publishes a few hundred of them, which spent the entry budget
    /// and, on Windows, launched one `icacls` per file. The bus key
    /// (`<home>/bus/secret`) is not under these directories and stays scanned.
    fn is_bus_endpoint_metadata(dir: &Path) -> bool {
        let is_metadata = dir
            .file_name()
            .is_some_and(|name| name == "topics" || name == "prefixes" || name == "caps");
        is_metadata
            && dir
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .is_some_and(|name| name == "bus")
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
                    if !Self::is_bus_endpoint_metadata(&path) {
                        stack.push(path);
                    }
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
    fn scan_for_exfiltration_risks(substrate_home: &Path) -> Vec<String> {
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
                    if !Self::is_bus_endpoint_metadata(&path) {
                        stack.push(path);
                    }
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
                if let Some(finding) = Self::acl_finding(&path, Self::windows_acl_listing(&path)) {
                    findings.push(finding);
                }
            }
        }
        findings
    }

    /// The readiness finding for one credential file, from its access-list
    /// `reading`: `Some` when the list grants broad access or cannot be
    /// trusted, `None` only for a list that was read in full and is
    /// restrictive (or for a file that vanished mid-scan). An unreadable list
    /// is a finding, not a clean result: a scan that treated "could not read"
    /// as "nothing exposed" would pass a file whose DACL holds an entry this
    /// reader does not understand.
    #[cfg(any(not(unix), test))]
    fn acl_finding(path: &Path, reading: std::io::Result<String>) -> Option<String> {
        match reading {
            Ok(listing) if Self::windows_acl_is_overly_permissive(&listing) => {
                Some(path.display().to_string())
            }
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(format!(
                "{} (access list unverifiable: {error})",
                path.display()
            )),
        }
    }

    /// `path`'s access list in `icacls` notation (one `principal:(flags)(rights)`
    /// line per allow or deny entry), read natively. This scan used to run
    /// `icacls <path>` once per credential file; from the windowless daemon
    /// each run opened a console window (a test home's `node.key` did so on
    /// 2026-11-09). An error means the list could not be read in full
    /// (including an entry type the reader does not understand); the caller
    /// reports that rather than treating the file as safe.
    #[cfg(windows)]
    fn windows_acl_listing(path: &Path) -> std::io::Result<String> {
        let entries = match winapi::acl::read_dacl(path)? {
            Some(entries) => entries,
            // A NULL DACL grants everyone full access.
            None => {
                return Ok(Self::icacls_entry_line(
                    "S-1-1-0",
                    false,
                    false,
                    0x001F_01FF,
                ));
            }
        };
        Ok(entries
            .iter()
            .map(|e| Self::icacls_entry_line(&e.sid, e.deny, e.inherited, e.mask))
            .collect::<Vec<_>>()
            .join(
                "
",
            ))
    }

    /// Off Windows and Unix there is no reader: report that, not a clean list.
    #[cfg(not(any(unix, windows)))]
    fn windows_acl_listing(_path: &Path) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no access-list reader on this platform",
        ))
    }

    /// One access-list entry the way `icacls` prints it, so
    /// [`Self::windows_acl_is_overly_permissive`] judges native readings and
    /// `icacls` output alike. Well-known broad principals carry the names
    /// `icacls` prints on an English host; every other trustee prints as
    /// `*<SID>`, which matches no broad principal (and, unlike a localized or
    /// user-chosen account name, cannot match one by accident).
    #[cfg(any(windows, test))]
    pub(crate) fn icacls_entry_line(sid: &str, deny: bool, inherited: bool, mask: u32) -> String {
        let principal = match sid {
            "S-1-1-0" => "Everyone".to_string(),
            "S-1-5-11" => "NT AUTHORITY\\Authenticated Users".to_string(),
            "S-1-5-32-545" => "BUILTIN\\Users".to_string(),
            "S-1-5-32-546" => "BUILTIN\\Guests".to_string(),
            // The built-in Guest account: relative id 501 of a machine or
            // domain (`S-1-5-21-<three ids>-501`).
            guest if guest.starts_with("S-1-5-21-") && guest.ends_with("-501") => {
                "Guest".to_string()
            }
            other => format!("*{other}"),
        };
        format!(
            "{principal}:{}{}({})",
            if inherited { "(I)" } else { "" },
            if deny { "(DENY)" } else { "" },
            Self::icacls_rights(mask)
        )
    }

    /// An access mask in `icacls` rights notation: generic bits mapped to
    /// their file rights (as the access check maps them), then the simple
    /// rights `F`, `M`, `RX`/`R`, `W`, `D` that the mask contains, then each
    /// remaining specific right, in `icacls`'s order. Matches `icacls` on
    /// Windows 11 for every combination checked (see the unit test).
    #[cfg(any(windows, test))]
    pub(crate) fn icacls_rights(mask: u32) -> String {
        const FILE_GENERIC_READ: u32 = 0x0012_0089;
        const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
        const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
        const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
        const SIMPLE: [(&str, u32); 6] = [
            ("F", FILE_ALL_ACCESS),
            ("M", 0x0013_01BF),
            ("RX", 0x0012_00A9),
            ("R", 0x0012_0089),
            ("W", 0x0010_0116),
            ("D", 0x0011_0000),
        ];
        const SPECIFIC: [(&str, u32); 16] = [
            ("DE", 0x0001_0000),
            ("Rc", 0x0002_0000),
            ("WDAC", 0x0004_0000),
            ("WO", 0x0008_0000),
            ("S", 0x0010_0000),
            ("AS", 0x0100_0000),
            ("MA", 0x0200_0000),
            ("RD", 0x0000_0001),
            ("WD", 0x0000_0002),
            ("AD", 0x0000_0004),
            ("REA", 0x0000_0008),
            ("WEA", 0x0000_0010),
            ("X", 0x0000_0020),
            ("DC", 0x0000_0040),
            ("RA", 0x0000_0080),
            ("WA", 0x0000_0100),
        ];
        let mut mask = mask;
        for (generic, rights) in [
            (0x1000_0000, FILE_ALL_ACCESS),
            (0x8000_0000, FILE_GENERIC_READ),
            (0x4000_0000, FILE_GENERIC_WRITE),
            (0x2000_0000, FILE_GENERIC_EXECUTE),
        ] {
            if mask & generic != 0 {
                mask = (mask & !generic) | rights;
            }
        }
        let mut rest = mask;
        let mut parts = Vec::new();
        for (name, bits) in SIMPLE {
            // `RX` covers `R`; a right whose bits are all taken adds nothing.
            if mask & bits == bits && rest & bits != 0 && !(name == "R" && parts.contains(&"RX")) {
                parts.push(name);
                rest &= !bits;
            }
        }
        for (name, bits) in SPECIFIC {
            if rest & bits == bits {
                parts.push(name);
                rest &= !bits;
            }
        }
        if parts.is_empty() {
            "N".to_string()
        } else {
            parts.join(",")
        }
    }

    /// Inspect `icacls` output and return `true` when broad non-owner principals
    /// (`Everyone`, `BUILTIN\Users`, `Authenticated Users`, `Guests`) are granted
    /// write, modify, or full-access permissions (`(F)`, `(M)`, `(W)`), or when
    /// `Everyone` is granted read access on a credential file.
    #[cfg(any(not(unix), test))]
    #[must_use]
    pub(crate) fn windows_acl_is_overly_permissive(icacls_output: &str) -> bool {
        const BROAD_PRINCIPALS: &[&str] = &[
            "everyone:",
            "builtin\\users:",
            "nt authority\\authenticated users:",
            "authenticated users:",
            "builtin\\guests:",
            "guest:",
        ];
        for raw_line in icacls_output.lines() {
            let line = raw_line.trim().to_ascii_lowercase();
            if line.is_empty() || line.starts_with("successfully processed") {
                continue;
            }
            for principal in BROAD_PRINCIPALS {
                if let Some(idx) = line.find(principal) {
                    let perms = &line[idx + principal.len()..];
                    if *principal == "everyone:"
                        || perms.contains("(f)")
                        || perms.contains("(m)")
                        || perms.contains("(w)")
                        || perms.contains("(rx,w)")
                    {
                        return true;
                    }
                }
            }
        }
        false
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
    home: std::path::PathBuf,
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
            home: home.to_path_buf(),
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

    /// CPU pressure for the resource governor, 0-100: the 1-minute load per
    /// logical CPU where the OS keeps a load average (Linux, macOS), the
    /// utilization sampled since the previous tick on Windows.
    fn cpu_pct() -> f64 {
        crate::susi_config::host_metrics::cpu_pressure_pct().map_or(0.0, f64::from)
    }

    /// Physical memory in use, 0-100, on every OS (0 when unreported).
    fn mem_pct() -> f64 {
        crate::susi_config::host_metrics::memory().map_or(0.0, |mem| mem.used_pct())
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

        // Estate reconciliation on its own cadence (default 5 min,
        // SUSI_ESTATE_INTERVAL_SECS): observe the local estate, diff it
        // against the declared desired state, act within policy — warm or
        // evict a model, start or drain a leaf runtime, rotate away from
        // a dead credential, collect orphan weights — and sign every pass
        // into the audit chain. The pass journal makes a crash a resume,
        // not a restart of the work (VC-202-024, T-DEEPSEEK-160).
        let estate = crate::eco_probe_scheduler::Subject {
            id: "estate_reconcile".to_string(),
            interval_secs: crate::estate_loop::interval_secs(),
            budget: 8,
            needs_consent: false, // everything it touches is daemon-local
        };
        if let crate::eco_probe_scheduler::Decision::Run { .. } =
            self.probes.decide(&estate, now, cond)
        {
            match crate::estate_loop::run_pass(&self.home, now) {
                Ok(report) => {
                    self.probes.record_run(&estate, now, report.receipts.len());
                    self.changes_last = self.changes_last.saturating_add(
                        report
                            .receipts
                            .iter()
                            .filter(|r| {
                                matches!(
                                    r.verdict,
                                    crate::estate_loop::Verdict::Applied(_)
                                        | crate::estate_loop::Verdict::Failed(_)
                                )
                            })
                            .count() as u32,
                    );
                }
                Err(e) => {
                    warn!("[runtime-admin] estate reconciliation pass failed: {e}");
                }
            }
        }

        // Only explicitly enabled background diagnostics run the full ladder.
        // Otherwise the first maintenance tick would run every rung immediately.
        // The full intent ladder has its own cadence (default daily,
        // SUSI_INTENT_LADDER_INTERVAL_SECS): every rung — trivial to
        // extreme — runs hermetically under its own budget, and the
        // scorecard is the reproducible record of what susi can do
        // from cold (VC-201-001, T-DEEPSEEK-200).
        let ladder = crate::eco_probe_scheduler::Subject {
            id: "intent_ladder".to_string(),
            interval_secs: susi_gawd::intent_ladder::schedule_secs(),
            budget: 8,
            needs_consent: false, // scratch HOME + scrubbed env: no estate, no egress
        };
        if background_diagnostics_requested()
            && matches!(
                self.probes.decide(&ladder, now, cond),
                crate::eco_probe_scheduler::Decision::Run { .. }
            )
        {
            match susi_gawd::intent_ladder::full_run(&self.home, &self.home) {
                Ok(card) => {
                    self.probes.record_run(&ladder, now, card.entries.len());
                    self.changes_last = self.changes_last.saturating_add(
                        card.entries.iter().filter(|e| e.verdict == "fail").count() as u32,
                    );
                    // Every failing rung lands in the host outbox —
                    // deduplicated, triaged by grade, rate-limited — for an
                    // agent to adopt as an evidence-backed repair task whose
                    // close is the intent passing again (VC-201-012).
                    if let Err(e) = susi_gawd::intent_ladder::record_gaps(&self.home, &card) {
                        warn!("[runtime-admin] intent ladder gap outbox: {e}");
                    }
                }
                Err(e) => warn!("[runtime-admin] intent ladder run failed: {e}"),
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
    fn first_goal_background_diagnostics_require_explicit_opt_in() {
        for setting in [None, Some(""), Some("0"), Some("false"), Some("true")] {
            assert!(!background_diagnostics_enabled(setting));
        }
        assert!(background_diagnostics_enabled(Some("1")));
    }

    #[test]
    fn no_stress_yields_no_pheromone() {
        let snapshot = TelemetrySnapshot::empty();
        assert!(SusiRuntimeAdmin::stress_pheromone_from_snapshot(&snapshot).is_none());
    }

    #[test]
    fn high_load_deposits_an_observation_pheromone() {
        let snapshot = TelemetrySnapshot {
            load_avg_1m: Some(99.0),
            ..TelemetrySnapshot::empty()
        };
        let pheromone = SusiRuntimeAdmin::stress_pheromone_from_snapshot(&snapshot).unwrap();
        assert_eq!(pheromone.topic, "hardware.stress");
        assert_eq!(pheromone.kind, PheromoneKind::Observation);
        assert_eq!(pheromone.payload["load_stress"], serde_json::json!(true));
    }

    #[test]
    fn saturated_cpu_without_a_load_average_is_load_stress() {
        // Windows: no load average, so the sampled utilization decides.
        let busy = TelemetrySnapshot {
            cpu_util_pct: Some(99.0),
            ..TelemetrySnapshot::empty()
        };
        let pheromone = SusiRuntimeAdmin::stress_pheromone_from_snapshot(&busy).unwrap();
        assert_eq!(pheromone.payload["load_stress"], serde_json::json!(true));
        let calm = TelemetrySnapshot {
            cpu_util_pct: Some(40.0),
            ..TelemetrySnapshot::empty()
        };
        assert!(SusiRuntimeAdmin::stress_pheromone_from_snapshot(&calm).is_none());
        // Where a load average exists it stays the authority.
        let loaded_os = TelemetrySnapshot {
            load_avg_1m: Some(0.5),
            cpu_util_pct: Some(99.0),
            ..TelemetrySnapshot::empty()
        };
        assert!(SusiRuntimeAdmin::stress_pheromone_from_snapshot(&loaded_os).is_none());
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

    #[test]
    fn windows_acl_parser_detects_overly_permissive_grants() {
        let exposed_everyone = "C:\\susi\\cluster.key Everyone:(I)(F)\r\n\
                                NT AUTHORITY\\SYSTEM:(I)(F)\r\n\
                                Successfully processed 1 files; Failed processing 0 files";
        assert!(SusiRuntimeAdmin::windows_acl_is_overly_permissive(
            exposed_everyone
        ));

        let exposed_users_write = "C:\\susi\\api.token BUILTIN\\Users:(I)(M)\r\n\
                                   DESKTOP\\user:(I)(F)\r\n";
        assert!(SusiRuntimeAdmin::windows_acl_is_overly_permissive(
            exposed_users_write
        ));

        let locked_down = "C:\\susi\\cluster.key DESKTOP\\user:(F)\r\n\
                           NT AUTHORITY\\SYSTEM:(I)(F)\r\n\
                           BUILTIN\\Administrators:(I)(F)\r\n\
                           BUILTIN\\Users:(I)(R)\r\n\
                           Successfully processed 1 files; Failed processing 0 files";
        assert!(!SusiRuntimeAdmin::windows_acl_is_overly_permissive(
            locked_down
        ));
    }

    /// Give `path` the access list `icacls <path> /grant *S-1-1-0:F` gave: the
    /// current user and Everyone, both with full control, set natively (no
    /// `icacls` process, which would open a console window).
    #[cfg(windows)]
    fn grant_everyone_full_control(path: &Path) {
        let user = winapi::acl::current_user_sid().unwrap();
        winapi::acl::set_protected_dacl(
            path,
            &[
                (user.as_str(), winapi::acl::FILE_ALL_ACCESS),
                ("S-1-1-0", winapi::acl::FILE_ALL_ACCESS),
            ],
        )
        .unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn readiness_scan_flags_overly_permissive_windows_key_files() {
        let dir =
            std::env::temp_dir().join(format!("susi_win_readiness_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("cluster.key");
        std::fs::write(&key_path, b"secret").unwrap();

        grant_everyone_full_control(&key_path);
        let findings = SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir);
        assert_eq!(findings.len(), 1);

        winapi::acl::restrict_to_current_user(&key_path).unwrap();
        assert!(SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Plant a DACL whose second entry is an `ACCESS_ALLOWED_CALLBACK_ACE`
    /// (type 9) for Everyone, which the native reader cannot list. The first
    /// entry is the ordinary owner-only grant, so a reader that skipped the
    /// callback entry would see exactly what `restrict_to_current_user` makes.
    #[cfg(windows)]
    #[allow(unsafe_code)] // test fixture: a DACL entry type the safe facade cannot write
    fn plant_callback_ace_for_everyone(path: &Path) {
        use winapi::shared::minwindef::DWORD;
        use winapi::um::accctrl::SE_FILE_OBJECT;
        use winapi::um::aclapi::SetNamedSecurityInfoW;
        use winapi::um::securitybaseapi::{AddAce, InitializeAcl};
        // ACL_REVISION_DS: callback entries need it.
        const ACL_REVISION_DS: DWORD = 4;
        use winapi::um::winnt::{
            ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        };

        // Owner-only first, so the file is protected and readable; then the
        // callback entry is added to a rebuilt list below.
        let user_ace = |ace_type: u8, mask: u32, sid: &[u8]| {
            let size = (8 + sid.len() + 3) & !3;
            let mut ace = vec![ace_type, 0];
            ace.extend_from_slice(&(size as u16).to_le_bytes());
            ace.extend_from_slice(&mask.to_le_bytes());
            ace.extend_from_slice(sid);
            ace.resize(size, 0);
            ace
        };
        // S-1-1-0 (Everyone): revision 1, one sub-authority, authority 1, id 0.
        let everyone = [1u8, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        let owner = winapi::acl::current_user_sid().unwrap();
        let owner_sid = {
            let mut parts = owner.split('-').skip(1);
            let revision: u8 = parts.next().unwrap().parse().unwrap();
            let authority: u64 = parts.next().unwrap().parse().unwrap();
            let subs: Vec<u32> = parts.map(|p| p.parse().unwrap()).collect();
            let mut bytes = vec![revision, subs.len() as u8];
            bytes.extend_from_slice(&authority.to_be_bytes()[2..]);
            for sub in subs {
                bytes.extend_from_slice(&sub.to_le_bytes());
            }
            bytes
        };
        let aces = [
            user_ace(0, winapi::acl::FILE_ALL_ACCESS, &owner_sid),
            user_ace(9, 0x0012_0089, &everyone),
        ];
        let size = (std::mem::size_of::<ACL>() + aces.iter().map(Vec::len).sum::<usize>() + 3) & !3;
        let mut acl_buf = vec![0u32; size / 4];
        let acl = acl_buf.as_mut_ptr().cast::<ACL>();
        let mut name: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
            .chain(Some(0))
            .collect();
        // SAFETY: `acl_buf` is a DWORD-aligned writable buffer of `size`
        // bytes living past every call; each entry is a complete ACE owned by
        // `aces` that AddAce copies; `name` is a NUL-terminated UTF-16 path;
        // only the DACL is set, the other security pointers are null.
        let status = unsafe {
            assert_ne!(InitializeAcl(acl, size as DWORD, ACL_REVISION_DS), 0);
            for ace in &aces {
                assert_ne!(
                    AddAce(
                        acl,
                        ACL_REVISION_DS,
                        DWORD::MAX,
                        ace.as_ptr().cast_mut().cast(),
                        ace.len() as DWORD,
                    ),
                    0
                );
            }
            SetNamedSecurityInfoW(
                name.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, 0, "SetNamedSecurityInfoW failed");
    }

    #[test]
    #[cfg(windows)]
    fn readiness_scan_flags_a_key_file_whose_access_list_cannot_be_read_in_full() {
        let dir =
            std::env::temp_dir().join(format!("susi_win_unverifiable_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("cluster.key");
        std::fs::write(&key_path, b"secret").unwrap();

        // Control: owner-only reads clean.
        winapi::acl::restrict_to_current_user(&key_path).unwrap();
        assert!(SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir).is_empty());

        // The same file with a callback entry the reader cannot list is a
        // finding, never a clean scan, and says why.
        plant_callback_ace_for_everyone(&key_path);
        let error =
            winapi::acl::read_dacl(&key_path).expect_err("callback entry must fail the read");
        assert_eq!(
            winapi::acl::unsupported_ace(&error),
            Some(&winapi::acl::UnsupportedAce {
                index: 1,
                ace_type: 9
            })
        );
        let findings = SusiRuntimeAdmin::scan_for_exfiltration_risks(&dir);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains("cluster.key"), "{findings:?}");
        assert!(findings[0].contains("unverifiable"), "{findings:?}");
        assert!(
            findings[0].contains("unsupported type 0x09"),
            "{findings:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_access_list_is_a_finding_and_only_a_vanished_file_is_not() {
        let path = Path::new("home/cluster.key");
        // Read in full and restrictive: clean.
        assert_eq!(
            SusiRuntimeAdmin::acl_finding(path, Ok("*S-1-5-18:(F)".to_string())),
            None
        );
        // Read in full and broad: the path alone.
        assert_eq!(
            SusiRuntimeAdmin::acl_finding(path, Ok("Everyone:(F)".to_string())),
            Some(path.display().to_string())
        );
        // Not read in full: reported with the reason, whatever the reason.
        for kind in [
            std::io::ErrorKind::Unsupported,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::Other,
        ] {
            let finding = SusiRuntimeAdmin::acl_finding(
                path,
                Err(std::io::Error::new(kind, "entry 1 is unreadable")),
            )
            .expect("an unreadable list must not read as clean");
            assert!(finding.starts_with("home"), "{finding}");
            assert!(finding.contains("access list unverifiable"), "{finding}");
            assert!(finding.contains("entry 1 is unreadable"), "{finding}");
        }
        // A file deleted between listing and reading has nothing to expose.
        assert_eq!(
            SusiRuntimeAdmin::acl_finding(
                path,
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            ),
            None
        );
    }

    #[test]
    fn native_access_masks_print_in_icacls_notation() {
        for (mask, expected) in [
            (0x001F_01FF, "F"),
            (0x1000_0000, "F"),
            (0x0013_01BF, "M"),
            (0x0012_00A9, "RX"),
            (0x0012_0089, "R"),
            (0x8000_0000, "R"),
            (0x0010_0116, "W"),
            (0x0000_0001, "RD"),
            (0x0000_0000, "N"),
        ] {
            assert_eq!(SusiRuntimeAdmin::icacls_rights(mask), expected, "{mask:#x}");
        }
    }

    #[test]
    fn native_entries_print_as_icacls_lines_the_parser_judges() {
        let everyone = SusiRuntimeAdmin::icacls_entry_line("S-1-1-0", false, false, 0x001F_01FF);
        assert_eq!(everyone, "Everyone:(F)");
        assert!(SusiRuntimeAdmin::windows_acl_is_overly_permissive(
            &everyone
        ));
        assert_eq!(
            SusiRuntimeAdmin::icacls_entry_line("S-1-5-32-545", false, true, 0x0012_00A9),
            r"BUILTIN\Users:(I)(RX)"
        );
        assert_eq!(
            SusiRuntimeAdmin::icacls_entry_line("S-1-5-21-1-2-3-501", false, false, 0x0012_0089),
            "Guest:(R)"
        );
        // An unnamed trustee prints as `*<SID>` and is never a broad principal.
        let system = SusiRuntimeAdmin::icacls_entry_line("S-1-5-18", false, false, 0x001F_01FF);
        assert_eq!(system, "*S-1-5-18:(F)");
        assert!(!SusiRuntimeAdmin::windows_acl_is_overly_permissive(&system));
        assert_eq!(
            SusiRuntimeAdmin::icacls_entry_line("S-1-5-21-1-2-3-1001", true, true, 0x0010_0116),
            "*S-1-5-21-1-2-3-1001:(I)(DENY)(W)"
        );
    }

    #[test]
    fn bus_endpoint_metadata_dirs_are_recognised_only_under_bus_pid() {
        let home = Path::new("susi-home");
        for kind in ["topics", "prefixes", "caps"] {
            assert!(SusiRuntimeAdmin::is_bus_endpoint_metadata(
                &home.join("bus").join("33832").join(kind)
            ));
        }
        // The bus key's directory and a pid directory are not metadata.
        assert!(!SusiRuntimeAdmin::is_bus_endpoint_metadata(
            &home.join("bus")
        ));
        assert!(!SusiRuntimeAdmin::is_bus_endpoint_metadata(
            &home.join("bus").join("33832")
        ));
        // Same names elsewhere stay in scope.
        assert!(!SusiRuntimeAdmin::is_bus_endpoint_metadata(
            &home.join("topics")
        ));
        assert!(!SusiRuntimeAdmin::is_bus_endpoint_metadata(
            &home.join("keys").join("33832").join("caps")
        ));
    }

    /// A home with plane-bus endpoint files named after sensitive-looking
    /// topics, the bus key, and a real key file; returns (home, bus key, key).
    fn home_with_bus_metadata(
        tag: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!(
            "susi_readiness_bus_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let pid_dir = home.join("bus").join("33832");
        for (kind, name) in [
            ("topics", "capability.tool.cluster_rekey_commit"),
            ("prefixes", "token.refresh."),
            ("caps", "secret_rotate"),
        ] {
            let dir = if kind == "caps" {
                pid_dir.join(kind).join("tool")
            } else {
                pid_dir.join(kind)
            };
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), br#"{"endpoint":"127.0.0.1:1","key":"x"}"#).unwrap();
        }
        let bus_key = home.join("bus").join("secret");
        std::fs::write(&bus_key, b"0123").unwrap();
        let key = home.join("cluster.key");
        std::fs::write(&key, b"secret").unwrap();
        (home, bus_key, key)
    }

    #[test]
    #[cfg(unix)]
    fn readiness_scan_skips_bus_endpoint_metadata_but_not_the_bus_key() {
        use std::os::unix::fs::PermissionsExt;

        let (home, bus_key, key) = home_with_bus_metadata("unix");
        let open = |path: &Path| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        };
        for entry in walk(&home) {
            open(&entry);
        }
        let mut findings = SusiRuntimeAdmin::scan_for_exfiltration_risks(&home);
        findings.sort();
        let mut expected = vec![bus_key.display().to_string(), key.display().to_string()];
        expected.sort();
        assert_eq!(findings, expected);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    #[test]
    #[cfg(windows)]
    fn readiness_scan_skips_bus_endpoint_metadata_but_not_the_bus_key() {
        let (home, bus_key, key) = home_with_bus_metadata("win");
        let grant_everyone = |path: &Path| {
            grant_everyone_full_control(path);
            true
        };
        let topic = home
            .join("bus")
            .join("33832")
            .join("topics")
            .join("capability.tool.cluster_rekey_commit");
        if grant_everyone(&topic) && grant_everyone(&bus_key) && grant_everyone(&key) {
            let mut findings = SusiRuntimeAdmin::scan_for_exfiltration_risks(&home);
            findings.sort();
            let mut expected = vec![bus_key.display().to_string(), key.display().to_string()];
            expected.sort();
            assert_eq!(findings, expected);
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
