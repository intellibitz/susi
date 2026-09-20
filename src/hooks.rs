//! Engine hooks implementation for extending susi-engine functionality

use std::path::Path;
use susi_error::EaiResult;
use susi_gemi::hardware::HardwareProfiler;
use susi_gmcp::tools::{bootstrap_registry, ToolRegistry};

/// Main hooks implementation for the susi-engine
pub struct SusiEngineHooks;

impl susi_tools::EngineHooks for SusiEngineHooks {
    /// Returns the current engine version
    fn engine_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Captures a snapshot of the current hardware state
    fn hardware_snapshot(&self) -> susi_tools::HardwareSnapshot {
        let profile = HardwareProfiler::get_profile();
        susi_tools::HardwareSnapshot {
            available_ram_gb: profile.available_ram_gb,
            acceleration_active: profile.acceleration_active,
        }
    }

    /// Attempts to synthesize a WASM reflex for missing capabilities
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

    /// Broadcasts a lock request to the supervisor
    fn broadcast_lock_request(&self, resource_id: &str) -> bool {
        susi_gawd::amas::SusiSupervisor::broadcast_lock_request(resource_id)
    }

    /// Bootstraps the tool registry with default tools
    fn bootstrap_tools(&self, registry: &ToolRegistry) {
        bootstrap_registry(registry);
    }
}
