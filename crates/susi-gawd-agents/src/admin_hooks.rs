//! Host-admin seam so agents never depend on `susi-gawd` host modules.
//!
//! `AdminAgent` / code-review / evolution agents route through [`AdminHooks`].
//! The host crate (`susi-gawd`) implements and registers them at startup
//! (same pattern as `susi_tools::EngineHooks`).

use std::path::Path;
use std::sync::OnceLock;
use susi_error::EaiResult;

pub trait AdminHooks: Send + Sync {
    fn enforce_version_consistency(&self, workspace: &Path) -> EaiResult<String>;
    fn audit_compliance(&self, workspace: &Path) -> EaiResult<String>;
    fn verify_version_alignment(&self, workspace: &Path) -> EaiResult<String>;
    fn execute_release(&self, workspace: &Path) -> EaiResult<String>;
    /// Rendered bloat-audit report (host owns `BloatAuditor`).
    fn bloat_audit_workspace(&self, workspace: &Path) -> EaiResult<String>;
    /// Drift / self-healing audit (host owns `EvolutionManager`).
    fn perform_autonomous_drift_audit(&self, workspace: &Path) -> EaiResult<String>;
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
        Ok("Substrate drift audit nominal (host hooks unwired).".to_string())
    }
}

pub(crate) fn hooks() -> &'static dyn AdminHooks {
    HOOKS.get_or_init(|| Box::new(NoOpAdminHooks)).as_ref()
}
