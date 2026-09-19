// susi-tools is a low-level crate: ToolRegistry's dispatch logic (self-healing
// capability provisioning, distributed lock broadcast, initial tool
// registration) genuinely needs gawd's reflex_synth/amas and gmcp's own
// CoreTools - but those crates depend on susi-tools too (gawd/gemi call
// ToolRegistry, gmcp's CoreTools implements SusiTool). A direct dependency
// back would recreate exactly the cycle this crate exists to break.
//
// Instead, the one real caller in a position to satisfy all of this (gmcp,
// which already legitimately depends on gawd/gemi) implements EngineHooks
// and main.rs wires it in once at startup via `init`. Everything in this
// crate that needs one of these capabilities goes through `hooks()`.

use crate::registry::ToolRegistry;
use std::path::Path;
use std::sync::OnceLock;
use susi_error::EaiResult;

pub struct HardwareSnapshot {
    pub available_ram_gb: usize,
    pub acceleration_active: bool,
}

pub trait EngineHooks: Send + Sync {
    /// The root `susi-engine` package version (`crate::SUSI_VERSION` at the
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

pub(crate) fn hooks() -> &'static dyn EngineHooks {
    HOOKS.get_or_init(|| Box::new(NoOpHooks)).as_ref()
}
