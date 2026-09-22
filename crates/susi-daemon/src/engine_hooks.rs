//! Host implementation of [`susi_tools::EngineHooks`].
//!
//! Lives in `susi-daemon` — the composition-root crate that legitimately sees
//! gawd + gemi + gmcp — so `susi-gmcp` stays a leaf protocol/tools crate with
//! no upward edge to the swarm host. Wired by
//! [`crate::composition::wire_engine_hooks`].

use std::path::Path;
use susi_error::EaiResult;
use susi_gemi::hardware::HardwareProfiler;

/// Host implementation of [`susi_tools::EngineHooks`].
pub struct SusiEngineHooks;

impl susi_tools::EngineHooks for SusiEngineHooks {
    fn engine_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn hardware_snapshot(&self) -> susi_tools::HardwareSnapshot {
        let profile = HardwareProfiler::get_profile();
        susi_tools::HardwareSnapshot {
            available_ram_gb: profile.available_ram_gb,
            acceleration_active: profile.acceleration_active,
        }
    }

    fn resolve_capability_gap(&self, server_name: &str, workspace: &Path) -> EaiResult<String> {
        match susi_gawd::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex(
            server_name,
            workspace,
        ) {
            Ok(wasm_path) => Ok(format!(
                "[HOT_PATCH] Synthesized and compiled a WASI reflex for '{}' at {}. Retry as 'reflex_{}'.",
                server_name, wasm_path, server_name
            )),
            Err(e) => Ok(format!(
                "[CAPABILITY_GAP] '{}' unresolved: no registry match, no installable package, \
                 and reflex synthesis failed ({}).",
                server_name, e
            )),
        }
    }

    fn broadcast_lock_request(&self, resource_id: &str) -> bool {
        susi_gawd::amas::SusiSupervisor::broadcast_lock_request(resource_id)
    }

    fn bootstrap_tools(&self, registry: &susi_tools::ToolRegistry) {
        susi_gmcp::tools::bootstrap_registry(registry);
    }

    fn audit_action(&self, tool: &str, detail: &str, workspace: &Path) -> EaiResult<()> {
        susi_gawd::safety::SafetyDetector::audit_action(tool, detail, workspace)?;
        susi_gawd::security::SecurityDetector::audit_action(tool, detail, workspace)
    }

    fn sanitize_input(&self, input: &str) -> EaiResult<String> {
        susi_gawd::ama::SusiMasterAgent::sanitize_input(input)
    }

    fn solve_mission(&self, intent: &str, workspace: &Path, version: &str) -> String {
        susi_gawd::ama::SusiMasterAgent::new().solve_clean(intent, workspace, version)
    }

    fn bloat_audit(&self, workspace: &Path) -> EaiResult<String> {
        let report = susi_gawd::bloat_audit::BloatAuditor::audit_workspace(workspace)?;
        Ok(susi_gawd::bloat_audit::BloatAuditor::render_report(&report))
    }

    fn identity_report(&self, workspace: &Path) -> EaiResult<String> {
        Ok(susi_gawd::self_core::identity_report(workspace))
    }

    fn audit_reasoning_substrate(&self, workspace: &Path) -> EaiResult<String> {
        susi_gawd::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(workspace)
    }

    fn self_validate(&self, workspace: &Path) -> EaiResult<String> {
        susi_gawd::self_validation::execute_autonomous_self_validation(workspace)
    }

    fn train_reflexes(&self, workspace: &Path) -> EaiResult<String> {
        susi_gawd::reflex_trainer::ReflexTrainer::force_train(workspace)
    }
}
