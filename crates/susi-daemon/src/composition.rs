//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::auto_discovery::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;

/// Register in-process plane-bus handlers for gemi / gawd / tools / agents.
///
/// The four planes live in separate crates with vendored bus copies. Each
/// `register` publishes a loopback endpoint under this process's bus
/// directory, so a request from any copy resolves. Sibling binaries
/// (`susi-gemi`, `susi-gmcp`, `susi-gawd`, `susi-dsh-cell`) are not part of
/// the release image; spawning them left every topic unanswered.
pub fn wire_plane_bus() {
    susi_gemi::plane_handler::register();
    susi_gawd::plane_handler::register();
    susi_tools::plane_handler::register();
    susi_agents::plane_handler::register();
    susi_gemi::http_provider::register_configured_cloud_endpoints(
        susi_gemi::susi_core::registry::CapabilityRegistry::global(),
    );
}

/// Wire `EngineHooks` so `ToolRegistry` and `susi-gmcp` tool handlers can reach
/// gawd/gemi without crate cycles. Safe to call more than once; later calls
/// are ignored by `OnceLock`.
pub fn wire_engine_hooks() {
    wire_plane_bus();
    // susi_tools::hooks::init(Box::new(crate::engine_hooks::SusiEngineHooks));
}

/// CLI composition root: hooks → extension packs → cloud.env → auto-prime.
///
/// Does not bind host-contract ports (daemon owns those). Call before command
/// dispatch that may touch tools, catalogs, or inference.
pub fn wire_cli_substrate(substrate: &Path) {
    wire_plane_bus();
    wire_engine_hooks();
    let _ = crate::susi_sandbox::extensions::ensure_extensions_substrate();
    let _ = std::fs::create_dir_all(substrate);
    susi_core::context_graph::ContextGraph::init_global_storage(
        substrate.join("context_graph.jsonl"),
    );
    crate::privacy::wire_mac_policy(substrate);
    crate::ambient::start_ambient_indexer(substrate);
    crate::auto_discovery::auto_prime_ecosystem(substrate);
}

#[cfg(test)]
mod tests {
    use super::wire_plane_bus;

    #[test]
    fn wire_plane_bus_answers_hardware_profile() {
        wire_plane_bus();
        let profile = susi_core::plane_bus::PlaneBus::global()
            .request(
                susi_core::plane_bus::topics::GEMI_HARDWARE_PROFILE,
                serde_json::json!({}),
            )
            .expect("gemi.hardware.profile handler");
        let cpus = profile.get("cpus").and_then(|v| v.as_u64()).unwrap_or(0);
        assert!(cpus > 0, "hardware profile had no cpus: {profile}");
    }
}
