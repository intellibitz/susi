//! Host-admin seam so agents never depend on `susi-gawd` host modules.
//!
//! `AdminAgent` / code-review / evolution agents route through [`AdminHooks`].
//! The host crate (`susi-gawd`) implements and registers them at startup
//! (same pattern as `susi_tools::EngineHooks`).

use crate::susi_error::EaiResult;
use std::path::Path;
use std::sync::OnceLock;

pub trait AdminHooks: Send + Sync {
    fn enforce_version_consistency(&self, workspace: &Path) -> EaiResult<String>;
    fn audit_compliance(&self, workspace: &Path) -> EaiResult<String>;
    fn verify_version_alignment(&self, workspace: &Path) -> EaiResult<String>;
    fn execute_release(&self, workspace: &Path) -> EaiResult<String>;
    /// Rendered bloat-audit report (host owns `BloatAuditor`).
    fn bloat_audit_workspace(&self, workspace: &Path) -> EaiResult<String>;
    /// Drift / self-healing audit (host owns `EvolutionManager`).
    fn perform_autonomous_drift_audit(&self, workspace: &Path) -> EaiResult<String>;
    /// Apply a workspace-confined patch-then-test cycle (host owns `patch_cycle`).
    /// `request_json` is a serialized `PatchRequest`. Returns serialized `PatchOutcome`.
    fn apply_patch_cycle(
        &self,
        workspace: &Path,
        request_json: &str,
        trust_level: &str,
    ) -> EaiResult<String>;
}

static HOOKS: OnceLock<Box<dyn AdminHooks>> = OnceLock::new();

/// Call once from the host (`susi-gawd`) before AdminAgent runs.
pub fn init(hooks: Box<dyn AdminHooks>) {
    let _ = HOOKS.set(hooks);
}

struct NoOpAdminHooks;
impl AdminHooks for NoOpAdminHooks {
    fn enforce_version_consistency(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] enforce_version_consistency: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn audit_compliance(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] audit_compliance: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn verify_version_alignment(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] verify_version_alignment: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn execute_release(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] execute_release: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn bloat_audit_workspace(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] bloat_audit_workspace: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn perform_autonomous_drift_audit(&self, _workspace: &Path) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] perform_autonomous_drift_audit: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
    fn apply_patch_cycle(
        &self,
        _workspace: &Path,
        _request_json: &str,
        _trust_level: &str,
    ) -> EaiResult<String> {
        Ok(
            "[ADMIN_UNWIRED] apply_patch_cycle: susi_gawd_agents::admin_hooks::init was never called."
                .to_string(),
        )
    }
}

static NO_OP_HOOKS: NoOpAdminHooks = NoOpAdminHooks;

pub fn hooks() -> &'static dyn AdminHooks {
    match HOOKS.get() {
        Some(hooks) => hooks.as_ref(),
        None => &NO_OP_HOOKS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn no_op_admin_hooks_returns_typed_unwired_diagnostics_for_all_actions() {
        let ws = Path::new(".");
        assert!(NO_OP_HOOKS
            .enforce_version_consistency(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] enforce_version_consistency"));
        assert!(NO_OP_HOOKS
            .audit_compliance(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] audit_compliance"));
        assert!(NO_OP_HOOKS
            .verify_version_alignment(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] verify_version_alignment"));
        assert!(NO_OP_HOOKS
            .execute_release(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] execute_release"));
        assert!(NO_OP_HOOKS
            .bloat_audit_workspace(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] bloat_audit_workspace"));
        assert!(NO_OP_HOOKS
            .perform_autonomous_drift_audit(ws)
            .unwrap()
            .contains("[ADMIN_UNWIRED] perform_autonomous_drift_audit"));
        assert!(NO_OP_HOOKS
            .apply_patch_cycle(ws, "{}", "local")
            .unwrap()
            .contains("[ADMIN_UNWIRED] apply_patch_cycle"));
    }

    struct StubAdminHooks;
    impl AdminHooks for StubAdminHooks {
        fn enforce_version_consistency(&self, workspace: &Path) -> EaiResult<String> {
            Ok(format!("synced:{}", workspace.display()))
        }
        fn audit_compliance(&self, workspace: &Path) -> EaiResult<String> {
            Ok(format!("compliant:{}", workspace.display()))
        }
        fn verify_version_alignment(&self, _workspace: &Path) -> EaiResult<String> {
            Ok("Version Alignment Verified (v1.0.0)".to_string())
        }
        fn execute_release(&self, workspace: &Path) -> EaiResult<String> {
            Ok(format!("release v1.0.0 from {}", workspace.display()))
        }
        fn bloat_audit_workspace(&self, _workspace: &Path) -> EaiResult<String> {
            Ok("0 Bloat Detected".to_string())
        }
        fn perform_autonomous_drift_audit(&self, _workspace: &Path) -> EaiResult<String> {
            Ok("0 Drift Detected".to_string())
        }
        fn apply_patch_cycle(
            &self,
            workspace: &Path,
            request_json: &str,
            trust_level: &str,
        ) -> EaiResult<String> {
            Ok(format!(
                "Patch Cycle Applied ({trust_level}) on {}: {request_json}",
                workspace.display()
            ))
        }
    }

    #[test]
    fn hooks_accessor_and_custom_admin_hooks_implementation_cover_all_seven_methods() {
        let ws = Path::new("workspace-root");
        let active = hooks();
        assert!(active.enforce_version_consistency(ws).is_ok());
        assert!(active.audit_compliance(ws).is_ok());
        assert!(active.verify_version_alignment(ws).is_ok());
        assert!(active.execute_release(ws).is_ok());
        assert!(active.bloat_audit_workspace(ws).is_ok());
        assert!(active.perform_autonomous_drift_audit(ws).is_ok());
        assert!(active.apply_patch_cycle(ws, "{}", "trusted").is_ok());

        let custom: &dyn AdminHooks = &StubAdminHooks;
        assert_eq!(
            custom.enforce_version_consistency(ws).unwrap(),
            "synced:workspace-root"
        );
        assert_eq!(
            custom.audit_compliance(ws).unwrap(),
            "compliant:workspace-root"
        );
        assert_eq!(
            custom.verify_version_alignment(ws).unwrap(),
            "Version Alignment Verified (v1.0.0)"
        );
        assert_eq!(
            custom.execute_release(ws).unwrap(),
            "release v1.0.0 from workspace-root"
        );
        assert_eq!(
            custom.bloat_audit_workspace(ws).unwrap(),
            "0 Bloat Detected"
        );
        assert_eq!(
            custom.perform_autonomous_drift_audit(ws).unwrap(),
            "0 Drift Detected"
        );
        assert_eq!(
            custom
                .apply_patch_cycle(ws, r#"{"files":[]}"#, "high")
                .unwrap(),
            r#"Patch Cycle Applied (high) on workspace-root: {"files":[]}"#
        );
    }

    struct FailingAdminHooks;
    impl AdminHooks for FailingAdminHooks {
        fn enforce_version_consistency(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("enforce failed"))
        }
        fn audit_compliance(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("audit failed"))
        }
        fn verify_version_alignment(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("verify failed"))
        }
        fn execute_release(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("release failed"))
        }
        fn bloat_audit_workspace(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("bloat failed"))
        }
        fn perform_autonomous_drift_audit(&self, _workspace: &Path) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("drift failed"))
        }
        fn apply_patch_cycle(
            &self,
            _workspace: &Path,
            _request_json: &str,
            _trust_level: &str,
        ) -> EaiResult<String> {
            Err(crate::susi_error::EaiError::internal("patch failed"))
        }
    }

    #[test]
    fn failing_admin_hooks_propagates_errors_across_all_seven_trait_methods() {
        let ws = Path::new("workspace-err");
        let failing: &dyn AdminHooks = &FailingAdminHooks;
        assert!(failing.enforce_version_consistency(ws).is_err());
        assert!(failing.audit_compliance(ws).is_err());
        assert!(failing.verify_version_alignment(ws).is_err());
        assert!(failing.execute_release(ws).is_err());
        assert!(failing.bloat_audit_workspace(ws).is_err());
        assert!(failing.perform_autonomous_drift_audit(ws).is_err());
        assert!(failing.apply_patch_cycle(ws, "{}", "local").is_err());
    }

    #[test]
    fn no_op_admin_hooks_messages_are_invariant_across_workspace_paths_and_trust_levels() {
        for (ws_str, trust) in [("/tmp/ws-a", "untrusted"), ("relative/ws-b", "elevated")] {
            let ws = Path::new(ws_str);
            assert_eq!(
                NO_OP_HOOKS.enforce_version_consistency(ws).unwrap(),
                "[ADMIN_UNWIRED] enforce_version_consistency: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.audit_compliance(ws).unwrap(),
                "[ADMIN_UNWIRED] audit_compliance: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.verify_version_alignment(ws).unwrap(),
                "[ADMIN_UNWIRED] verify_version_alignment: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.execute_release(ws).unwrap(),
                "[ADMIN_UNWIRED] execute_release: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.bloat_audit_workspace(ws).unwrap(),
                "[ADMIN_UNWIRED] bloat_audit_workspace: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.perform_autonomous_drift_audit(ws).unwrap(),
                "[ADMIN_UNWIRED] perform_autonomous_drift_audit: susi_gawd_agents::admin_hooks::init was never called."
            );
            assert_eq!(
                NO_OP_HOOKS.apply_patch_cycle(ws, r#"{"op":1}"#, trust).unwrap(),
                "[ADMIN_UNWIRED] apply_patch_cycle: susi_gawd_agents::admin_hooks::init was never called."
            );
        }
    }
}
