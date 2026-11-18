//! Host implementation of [`susi_tools::EngineHooks`].
//!
//! Lives in `susi-daemon` — the composition-root crate that legitimately sees
//! gawd + gemi + gmcp — so `susi-gmcp` stays a leaf protocol/tools crate with
//! no upward edge to the swarm host. Wired by
//! [`crate::composition::wire_engine_hooks`].

use std::path::Path;
use susi_gemi::models::hardware::HardwareProfiler;

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
        use susi_gawd::reflex_synth::ReflexSynthesizer;
        let outcome = ReflexSynthesizer::synthesize_capability(server_name, workspace);
        Ok(ReflexSynthesizer::gap_report(server_name, &outcome))
    }

    fn broadcast_lock_request(&self, resource_id: &str) -> bool {
        susi_gawd::amas::SusiSupervisor::broadcast_lock_request(resource_id)
    }

    fn bootstrap_tools(&self, registry: &susi_tools::ToolRegistry) {
        crate::gmcp_bootstrap::bootstrap_registry(registry);
    }

    fn tool_input_schema(&self, name: &str) -> Option<serde_json::Value> {
        susi_gmcp::tools::args::core_input_schema(name)
            .map(|schema| serde_json::Value::Object((*schema).clone()))
    }

    fn audit_action(
        &self,
        tool: &str,
        detail: &str,
        workspace: &Path,
    ) -> susi_tools::susi_error::EaiResult<()> {
        // `susi_gawd::safety`/`security` re-export susi-gawd-agents'
        // detectors; the error type is inferred because that path is not
        // nameable from this crate.
        susi_gawd::safety::SafetyDetector::audit_action(tool, detail, workspace)?;
        susi_gawd::security::SecurityDetector::audit_action(tool, detail, workspace)
    }

    fn sanitize_input(&self, input: &str) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::ama::SusiMasterAgent::sanitize_input(input)
    }

    fn solve_mission(&self, intent: &str, workspace: &Path, version: &str) -> String {
        susi_gawd::ama::SusiMasterAgent::new().solve_clean(intent, workspace, version)
    }

    fn bloat_audit(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        let report = susi_gawd::bloat_audit::BloatAuditor::audit_workspace(workspace)?;
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
    }

    fn self_validate(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::self_validation::execute_autonomous_self_validation(workspace)
    }

    fn train_reflexes(&self, workspace: &Path) -> susi_tools::susi_error::EaiResult<String> {
        susi_gawd::reflex_trainer::ReflexTrainer::force_train(workspace)
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use susi_tools::EngineHooks;

    #[test]
    fn engine_version_matches_cargo_pkg_version_and_is_non_empty() {
        let hooks = SusiEngineHooks;
        assert_eq!(hooks.engine_version(), env!("CARGO_PKG_VERSION"));
        assert!(!hooks.engine_version().is_empty());
    }

    #[test]
    fn built_in_tool_schema_bridge_returns_typed_arguments_only_for_core_tools() {
        let hooks = SusiEngineHooks;
        let schema = hooks
            .tool_input_schema("write_file")
            .expect("built-in tool schema");
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], serde_json::json!(["path", "content"]));
        assert!(hooks.tool_input_schema("mcp.example:remote_tool").is_none());
    }

    #[test]
    fn sanitize_input_passes_clean_text_through_master_agent() {
        let hooks = SusiEngineHooks;
        let out = hooks.sanitize_input("verify unit tests").unwrap();
        assert!(out.contains("verify unit tests"));
    }

    #[test]
    fn identity_report_returns_non_empty_substrate_identity_for_workspace() {
        let hooks = SusiEngineHooks;
        let dir = std::env::temp_dir().join(format!("susi-enghooks-id-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let report = hooks.identity_report(&dir).unwrap();
        assert!(!report.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_action_vetoes_destructive_command_and_allows_benign_read() {
        let hooks = SusiEngineHooks;
        let dir = std::env::temp_dir().join(format!("susi-enghooks-aud-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(hooks.audit_action("read_file", "README.md", &dir).is_ok());
        assert!(
            hooks
                .audit_action("exec_command", "rm -rf / --no-preserve-root", &dir)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardware_snapshot_and_broadcast_lock_request_execute_cleanly() {
        let hooks = SusiEngineHooks;
        let snap = hooks.hardware_snapshot();
        let _ = (snap.available_ram_gb, snap.acceleration_active);
        let _ = hooks.broadcast_lock_request("unit-test-resource-lock");
    }
}
