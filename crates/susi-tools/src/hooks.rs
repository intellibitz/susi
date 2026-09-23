// susi-tools is a low-level crate: ToolRegistry's dispatch logic (self-healing
// capability provisioning, distributed lock broadcast, initial tool
// registration) genuinely needs gawd's reflex_synth/amas and gmcp's own
// CoreTools - but those crates depend on susi-tools too (gawd/gemi call
// ToolRegistry, gmcp's CoreTools implements SusiTool). A direct dependency
// back would recreate exactly the cycle this crate exists to break.
//
// Instead, the composition root (`susi-daemon`, which legitimately depends on
// gawd/gemi/gmcp) implements EngineHooks (`susi_daemon::SusiEngineHooks`) and
// wires it once via `composition::wire_engine_hooks` / `wire_cli_substrate`.
// Everything in this crate - and the swarm-facing seam used by susi-gmcp's
// tool handlers - goes through `hooks()`.

use crate::registry::ToolRegistry;
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;
use std::sync::OnceLock;

pub struct HardwareSnapshot {
    pub available_ram_gb: usize,
    pub acceleration_active: bool,
}

pub trait EngineHooks: Send + Sync {
    /// The root `susi` package version (`crate::SUSI_VERSION` at the
    /// call site before this crate existed) - not this crate's own version.
    fn engine_version(&self) -> &'static str;
    fn hardware_snapshot(&self) -> HardwareSnapshot;
    /// No registry match and no installable package exist for a requested
    /// tool - last-resort autonomous hot-patch (was
    /// `gawd::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex`).
    fn resolve_capability_gap(&self, name: &str, workspace: &Path) -> EaiResult<String>;
    /// Distributed resource sovereignty (was
    /// `gawd::amas::SusiSupervisor::broadcast_lock_request`).
    fn broadcast_lock_request(&self, resource_id: &str) -> bool;
    /// Populate a freshly created registry with every concrete tool
    /// (`CoreTools::*` and synthesized reflexes) - was `ToolRegistry::bootstrap`.
    fn bootstrap_tools(&self, registry: &ToolRegistry);

    // ── Swarm-facing seam ────────────────────────────────────────────────
    // The following capabilities are owned by the gawd host (safety/security
    // detectors, SusiMasterAgent, trainers, identity). MCP tool handlers in
    // susi-gmcp call them through here so gmcp never imports susi-gawd.
    //
    // Defaults FAIL CLOSED: these guard action-capable tools (exec_command,
    // sandbox_exec, agents_run, reason, susi_solve). An unwired context must
    // never run them unaudited — same behavior as the pre-seam direct calls.

    /// Governance audit pair for an action-capable tool call (was
    /// `gawd::safety::SafetyDetector` + `gawd::security::SecurityDetector`
    /// `audit_action`).
    fn audit_action(&self, _tool: &str, _detail: &str, _workspace: &Path) -> EaiResult<()> {
        Err(unwired())
    }

    /// Intent sanitization (was `gawd::ama::SusiMasterAgent::sanitize_input`).
    fn sanitize_input(&self, _input: &str) -> EaiResult<String> {
        Err(unwired())
    }

    /// Full swarm mission solve (was `SusiMasterAgent::new().solve_clean`).
    fn solve_mission(&self, _intent: &str, _workspace: &Path, _version: &str) -> String {
        "[CAPABILITY_GAP] swarm substrate unwired: susi_tools::hooks::init was never called."
            .to_string()
    }

    /// Rendered bloat-audit report (was `gawd::bloat_audit::BloatAuditor`).
    fn bloat_audit(&self, _workspace: &Path) -> EaiResult<String> {
        Err(unwired())
    }

    /// Substrate identity report (was `AlphaSelf` inventory + `AlphaBrainContext`).
    fn identity_report(&self, _workspace: &Path) -> EaiResult<String> {
        Err(unwired())
    }

    /// Reasoning-substrate audit (was `gawd::reason_trainer::ReasoningTrainer`).
    fn audit_reasoning_substrate(&self, _workspace: &Path) -> EaiResult<String> {
        Err(unwired())
    }

    /// Autonomous self-validation (was `gawd::self_validation`).
    fn self_validate(&self, _workspace: &Path) -> EaiResult<String> {
        Err(unwired())
    }

    /// Reflex distillation (was `gawd::reflex_trainer::ReflexTrainer::force_train`).
    fn train_reflexes(&self, _workspace: &Path) -> EaiResult<String> {
        Err(unwired())
    }
}

fn unwired() -> EaiError {
    EaiError::governance("swarm substrate unwired: susi_tools::hooks::init was never called")
}

static HOOKS: OnceLock<Box<dyn EngineHooks>> = OnceLock::new();

/// Call once, early in `main()`, before any `ToolRegistry` use. `main()`
/// calling this as its first line always wins the race against `hooks()`'s
/// own fallback below, since nothing in this crate runs before `main()`
/// starts; a later call (there shouldn't be one) is silently ignored.
pub fn init(hooks: Box<dyn EngineHooks>) {
    let _ = HOOKS.set(hooks);
}

/// Inert fallback for contexts that never call `init` (unit tests, mainly -
/// they don't go through `main()`). Never panics; every capability degrades
/// to a harmless no-op/empty result instead of exercising real gawd/gemi
/// behavior. Production always wins the race in `init`'s doc comment above,
/// so this path is test-only in practice.
struct NoOpHooks;
impl EngineHooks for NoOpHooks {
    fn engine_version(&self) -> &'static str {
        "unknown"
    }
    fn hardware_snapshot(&self) -> HardwareSnapshot {
        HardwareSnapshot {
            available_ram_gb: 0,
            acceleration_active: false,
        }
    }
    fn resolve_capability_gap(&self, name: &str, _workspace: &Path) -> EaiResult<String> {
        Ok(format!(
            "[CAPABILITY_GAP] '{}' unresolved: susi_tools::hooks::init was never called.",
            name
        ))
    }
    fn broadcast_lock_request(&self, _resource_id: &str) -> bool {
        false
    }
    fn bootstrap_tools(&self, _registry: &ToolRegistry) {}
}

/// Access the wired hooks. Public so `susi-gmcp` tool handlers can reach the
/// swarm-facing seam above without importing `susi-gawd`.
pub fn hooks() -> &'static dyn EngineHooks {
    HOOKS.get_or_init(|| Box::new(NoOpHooks)).as_ref()
}
