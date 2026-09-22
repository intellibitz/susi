// SUSI Runtime Admin: Autonomous Substrate Administration & Drift Correction
// Reality Check Always On - Hardware-Aware Self-Tuning and Autonomous Experience Distillation

use std::path::Path;
use std::time::Duration;
use susi_error::EaiResult;
use susi_gemi::hardware::HardwareProfiler;
use susi_gemi::models::ModelManager;
use susi_sandbox::manager::SusiAuditLogger;
use tracing::info;

pub struct SusiRuntimeAdmin;

impl SusiRuntimeAdmin {
    /// Bootstraps the administrative substrate background cycle.
    /// `substrate_home` is the host substrate root (`~/.susi`), never a project cwd.
    pub fn start_administration_cycle(substrate_home: &Path) {
        let home = substrate_home.to_path_buf();
        std::thread::spawn(move || {
            // Mandate: Perform immediate Readiness Pulse on substrate boot
            let _ = Self::perform_substrate_audit(&home);
            let _ = Self::perform_host_readiness(&home);

            let mut last_pulse = std::time::Instant::now();
            loop {
                // 1. Hardware Load Watchdog (High-Resolution)
                Self::perform_hardware_watchdog_audit();

                // 2. Periodic host readiness (Every 5 minutes) — not project work
                if last_pulse.elapsed() > Duration::from_secs(300) {
                    let _ = Self::perform_substrate_audit(&home);
                    let _ = Self::perform_host_readiness(&home);
                    let _ = Self::consolidate_sovereign_memory(&home);
                    last_pulse = std::time::Instant::now();
                }

                std::thread::sleep(Duration::from_secs(10));
            }
        });
    }

    /// Hardware Watchdog: Autonomously adjusts substrate footprint based on system load.
    fn perform_hardware_watchdog_audit() {
        let profile = HardwareProfiler::get_profile();
        let load_parts: Vec<&str> = profile.load_avg.split(',').collect();
        if let Some(load_1m_str) = load_parts.first()
            && let Ok(load_1m) = load_1m_str.trim().parse::<f32>()
        {
            let cpu_threshold = profile.cpus as f32 * 0.85;
            if load_1m > cpu_threshold {
                // System is under stress. Ladder down concurrency.
                susi_gawd::agents::GawdAgentFleet::throttle_concurrency(true);
            } else {
                susi_gawd::agents::GawdAgentFleet::throttle_concurrency(false);
            }
        }
    }

    /// Autonomous Memory Consolidation: Distills recent missions into the PKB.
    fn consolidate_sovereign_memory(workspace: &Path) -> EaiResult<()> {
        info!("[Sovereign Mind] Consolidating mission experience into PKB...");
        let _ = susi_gawd::pkb::ProtocolKnowledgeBase::consolidate_recent_interactions(workspace);
        Ok(())
    }

    /// Host-only readiness (models, substrate safety). Never treats
    /// `substrate_home` as a coding project — project work is always cwd.
    pub fn perform_host_readiness(substrate_home: &Path) -> EaiResult<()> {
        let ama = susi_gawd::ama::SusiMasterAgent::new();

        info!("[Readiness] Auditing model substrate optimal state...");
        let _ = ModelManager::ensure_hardware_optimal_models(substrate_home);

        info!("[Readiness] Scanning substrate for exfiltration vectors...");
        let sec_res = ama.solve_clean(
            "admin pulse: scan workspace for high-risk exfiltration vectors and security leaks. Mask if found.",
            substrate_home,
            env!("CARGO_PKG_VERSION"),
        );
        if sec_res.contains("VIOLATION") || sec_res.contains("MASKED") {
            println!("\n[READINESS: SECURITY PROTOCOLS ENGAGED]");
            println!("{}\n", sec_res);
        }
        Ok(())
    }

    /// Hardware saturation audit, drift detection, and model substrate tuning.
    pub fn perform_substrate_audit(workspace: &Path) -> EaiResult<()> {
        let profile = HardwareProfiler::get_profile();

        // 1. Hardware Saturation Audit
        if profile.acceleration_active {
            SusiAuditLogger::log_event(
                workspace,
                "SUBSTRATE_AUDIT",
                "GPU Acceleration Verified Optimal.",
            );
        } else if profile.ram_gb >= 16 {
            SusiAuditLogger::log_event(
                workspace,
                "SUBSTRATE_AUDIT",
                "System RAM sufficient for high-fidelity CPU inference.",
            );
        }

        // 2. Autonomous Drift Detection
        let _ = susi_gawd::evolution::EvolutionManager::perform_autonomous_drift_audit(workspace);

        // 3. Model Substrate Tuning
        let _ = ModelManager::ensure_hardware_optimal_models(workspace);

        Ok(())
    }
}
