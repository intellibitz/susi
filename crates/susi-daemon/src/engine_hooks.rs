//! Host implementation of [`susi_tools::EngineHooks`].
//!
//! Lives in `susi-daemon` — the composition-root crate that legitimately sees
//! gawd + gemi + gmcp — so `susi-gmcp` stays a leaf protocol/tools crate with
//! no upward edge to the swarm host. Wired by
//! [`crate::composition::wire_engine_hooks`].

use std::path::Path;
use susi_gemi::hardware::HardwareProfiler;

/// Vendored-error boundary: downstream tiers return their own vendored
/// `EaiError` while `EngineHooks` expects susi-tools' vendored `EaiError`.
/// `rewrap` preserves the error kind across the boundary.
fn to_tools_err(e: susi_gawd::susi_error::EaiError) -> susi_tools::susi_error::EaiError {
    susi_tools::susi_error::rewrap(e.kind_name(), e.to_string())
}

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

    fn resolve_capability_gap(
        &self,
        server_name: &str,
        workspace: &Path,
    ) -> susi_tools::susi_error::EaiResult<String> {
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
        crate::gmcp_bootstrap::bootstrap_registry(registry);
    }

    fn audit_action(
        &self,
        tool: &str,
        detail: &str,
        workspace: &Path,
    ) -> susi_tools::susi_error::EaiResult<()> {
        // `susi_gawd::safety`/`security` re-export susi-gawd-agents' detectors,
        // so these return the agents vendored `EaiError` — inferred, since
        // that concrete type is not nameable from this crate.
        susi_gawd::safety::SafetyDetector::audit_action(tool, detail, workspace)
            .map_err(|e| susi_tools::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        susi_gawd::security::SecurityDetector::audit_action(tool, detail, workspace)
            .map_err(|e| susi_tools::susi_error::rewrap(e.kind_name(), e.to_string()))
    }

    fn sanitize_input(&self, input: &str) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::ama::SusiMasterAgent::sanitize_input(input).map_err(to_tools_err)
    }

    fn solve_mission(&self, intent: &str, workspace: &Path, version: &str) -> String {
        susi_gawd::ama::SusiMasterAgent::new().solve_clean(intent, workspace, version)
    }

    fn bloat_audit(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        let report = susi_gawd::bloat_audit::BloatAuditor::audit_workspace(workspace)
            .map_err(to_tools_err)?;
        Ok(susi_gawd::bloat_audit::BloatAuditor::render_report(&report))
    }

    fn identity_report(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        Ok(susi_gawd::self_core::identity_report(workspace))
    }

    fn audit_reasoning_substrate(
        &self,
        workspace: &Path,
    ) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(workspace)
            .map_err(to_tools_err)
    }

    fn self_validate(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::self_validation::execute_autonomous_self_validation(workspace)
            .map_err(to_tools_err)
    }

    fn train_reflexes(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::reflex_trainer::ReflexTrainer::force_train(workspace).map_err(to_tools_err)
    }
}
