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
    /// A requested tool is missing - first-party autonomous hot-patch; core
    /// never provisions a third-party package to close the gap (was
    /// `gawd::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex`).
    fn resolve_capability_gap(&self, name: &str, workspace: &Path) -> EaiResult<String>;
    /// Distributed resource sovereignty (was
    /// `gawd::amas::SusiSupervisor::broadcast_lock_request`).
    fn broadcast_lock_request(&self, resource_id: &str) -> bool;
    /// Populate a freshly created registry with every concrete tool
    /// (`CoreTools::*` and synthesized reflexes) - was `ToolRegistry::bootstrap`.
    fn bootstrap_tools(&self, registry: &ToolRegistry);

    /// Return the typed JSON Schema for a host-owned built-in tool. The low-
    /// level tools crate cannot depend on GMCP, so the composition root owns
    /// this lookup. Dynamic proxies return `None` rather than guessing.
    fn tool_input_schema(&self, _name: &str) -> Option<serde_json::Value> {
        None
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Implements only the required methods, so every swarm-facing method
    /// runs its trait default.
    struct MinimalHooks;
    impl EngineHooks for MinimalHooks {
        fn engine_version(&self) -> &'static str {
            "test"
        }
        fn hardware_snapshot(&self) -> HardwareSnapshot {
            HardwareSnapshot {
                available_ram_gb: 1,
                acceleration_active: true,
            }
        }
        fn resolve_capability_gap(&self, _name: &str, _workspace: &Path) -> EaiResult<String> {
            Ok(String::new())
        }
        fn broadcast_lock_request(&self, _resource_id: &str) -> bool {
            true
        }
        fn bootstrap_tools(&self, _registry: &ToolRegistry) {}
    }

    fn assert_fails_closed<T: std::fmt::Debug>(result: EaiResult<T>) {
        let err = result.expect_err("an unwired swarm seam must fail closed");
        assert!(
            err.to_string().contains("swarm substrate unwired"),
            "unexpected error: {err}"
        );
    }

    fn assert_every_default_fails_closed(h: &dyn EngineHooks) {
        let ws = Path::new(".");
        assert_fails_closed(h.audit_action("exec_command", "rm -rf /", ws));
        assert_fails_closed(h.sanitize_input("hello"));
        assert_fails_closed(h.bloat_audit(ws));
        assert_fails_closed(h.identity_report(ws));
        assert_fails_closed(h.audit_reasoning_substrate(ws));
        assert_fails_closed(h.self_validate(ws));
        assert_fails_closed(h.train_reflexes(ws));
        let solved = h.solve_mission("do something", ws, "0.0.0");
        assert!(solved.starts_with("[CAPABILITY_GAP]"), "{solved}");
        assert!(solved.contains("unwired"), "{solved}");
    }

    #[test]
    fn swarm_facing_trait_defaults_fail_closed_when_unwired() {
        assert_every_default_fails_closed(&MinimalHooks);
    }

    #[test]
    fn no_op_hooks_degrade_to_inert_results_and_still_fail_closed() {
        let h = NoOpHooks;
        assert_eq!(h.engine_version(), "unknown");
        let hw = h.hardware_snapshot();
        assert_eq!(hw.available_ram_gb, 0);
        assert!(!hw.acceleration_active);
        let gap = h
            .resolve_capability_gap("missing_tool", Path::new("."))
            .unwrap();
        assert!(gap.starts_with("[CAPABILITY_GAP] 'missing_tool' unresolved"));
        assert!(!h.broadcast_lock_request("resource-1"));
        assert_every_default_fails_closed(&h);
    }

    #[test]
    fn hooks_fallback_is_inert_when_init_was_never_called() {
        // The unit-test binary never calls `init`, so `hooks()` must resolve
        // to the inert fallback and keep the action-capable seam closed.
        let h = hooks();
        assert_eq!(h.engine_version(), "unknown");
        assert!(!h.broadcast_lock_request("resource-1"));
        assert_every_default_fails_closed(h);
        // Stable: repeated calls return the same wired instance.
        assert!(std::ptr::addr_eq(hooks(), h));
    }

    #[test]
    fn minimal_hooks_required_methods_dispatch_through_trait_object() {
        let h: &dyn EngineHooks = &MinimalHooks;
        assert_eq!(h.engine_version(), "test");
        let hw = h.hardware_snapshot();
        assert_eq!(hw.available_ram_gb, 1);
        assert!(hw.acceleration_active);
        let gap = h
            .resolve_capability_gap("x", Path::new("."))
            .expect("resolve_capability_gap on MinimalHooks");
        assert_eq!(gap, "");
        assert!(h.broadcast_lock_request("lock-1"));
    }

    #[test]
    fn solve_mission_and_resolve_capability_gap_preserve_requested_tool_name() {
        let ws = Path::new("/tmp/ws");
        let gap = NoOpHooks.resolve_capability_gap("tool_alpha", ws).unwrap();
        assert!(gap.contains("tool_alpha"));
        let solved = MinimalHooks.solve_mission("goal_beta", ws, "1.0.0");
        assert!(solved.starts_with("[CAPABILITY_GAP]"));
    }
}
