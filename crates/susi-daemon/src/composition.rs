//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::discovery_pipeline::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;
use std::sync::OnceLock;

/// Production swarm-host surface: orchestrator, watchdog, cell identity,
/// load balancer. Wired once from [`wire_engine_hooks`] so these modules
/// are reachable from the composition root rather than compiled-only.
struct SwarmHost {
    orchestrator: crate::orchestrator::Orchestrator,
    watchdog: crate::watchdog::WatchdogManager,
    identity: crate::identity::IdentityManager,
    balancer: crate::load_balancer::LoadBalancer,
}

static SWARM_HOST: OnceLock<SwarmHost> = OnceLock::new();

impl SwarmHost {
    fn new() -> Self {
        let orchestrator = crate::orchestrator::Orchestrator::new("susi-host");
        let watchdog = crate::watchdog::WatchdogManager::default();
        watchdog.register_cell("susi-host");
        let mut identity = crate::identity::IdentityManager::new();
        if let Ok((cell, _)) = crate::identity::IdentityManager::generate_identity("susi-host") {
            let _ = identity.register_identity(&cell);
        }
        let balancer = crate::load_balancer::LoadBalancer::new();
        balancer.register_cell("susi-host", 1);
        Self {
            orchestrator,
            watchdog,
            identity,
            balancer,
        }
    }
}

fn wire_swarm_host() {
    let _ = SWARM_HOST.get_or_init(SwarmHost::new);
}

/// Snapshot of the in-process swarm host for `susi os`. Wires the host
/// if the daemon/CLI composition root has not yet.
pub fn swarm_host_snapshot() -> serde_json::Value {
    wire_swarm_host();
    let Some(host) = SWARM_HOST.get() else {
        return serde_json::json!({ "wired": false });
    };
    let (workers, busy) = host.orchestrator.pool_summary();
    let _ = crate::scheduler::schedule_cells(&[], "", crate::scheduler::SchedulingStrategy::Greedy);
    let self_healing =
        crate::self_healing::SelfHealingManager::new(&susi_paths::SusiDirs::substrate_home())
            .is_ok();
    serde_json::json!({
        "wired": true,
        "orchestrator_id": host.orchestrator.orchestrator_id,
        "workers": workers,
        "busy": busy,
        "dead_cells": host.watchdog.find_dead_cells(),
        "identities": host.identity.registered_count(),
        "next_cell": host.balancer.next_cell(),
        "self_healing": self_healing,
    })
}

/// Register in-process plane-bus handlers for gemi / gawd / tools / agents.
///
/// The four planes live in separate crates over one shared bus. Each
/// `register` publishes a loopback endpoint under this process's bus
/// directory, so a request from any plane resolves. Sibling binaries
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
    wire_swarm_host();
    susi_tools::hooks::init(Box::new(crate::engine_hooks::SusiEngineHooks));
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
    // Catalog priming only: Swarm Cells are long-running servers owned by
    // the daemon, never spawned per CLI invocation.
    let _ = crate::discovery_pipeline::prime_catalogs(substrate);
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
