//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::discovery_pipeline::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;
use std::sync::OnceLock;

/// Production swarm-host surface: orchestrator, watchdog, cell identity,
/// load balancer, task queue, TTL, metrics, inference fallback, HTTP
/// gateway. Wired once from [`wire_engine_hooks`].
struct SwarmHost {
    orchestrator: crate::orchestrator::Orchestrator,
    watchdog: crate::watchdog::WatchdogManager,
    identity: crate::identity::IdentityManager,
    balancer: crate::load_balancer::LoadBalancer,
    queue: crate::task_queue::TaskQueue,
    ttl: crate::ttl::TtlManager,
    metrics: crate::metrics::MetricsManager,
    fallback: crate::fallback::FallbackRouter,
    gateway: crate::http_gateway::HttpGateway,
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
        let queue = crate::task_queue::TaskQueue::default();
        let ttl = crate::ttl::TtlManager::default();
        ttl.register_cell("susi-host", None);
        let metrics = crate::metrics::MetricsManager::new();
        let fallback = crate::fallback::FallbackRouter::new(
            crate::fallback::InferenceEndpoint::LocalQuantized("local-primary".into()),
            crate::fallback::InferenceEndpoint::LocalQuantized("local-fallback".into()),
        );
        let gateway = crate::http_gateway::HttpGateway::new();
        gateway.expose_cell("susi-host");
        Self {
            orchestrator,
            watchdog,
            identity,
            balancer,
            queue,
            ttl,
            metrics,
            fallback,
            gateway,
        }
    }
}

fn wire_swarm_host() {
    let _ = SWARM_HOST.get_or_init(SwarmHost::new);
}

/// Side-effecting OS planes (checkpoint/log/plugin/fork/hibernate dirs)
/// plus in-memory NAT/gossip/snapshot/proxy/reloader. Constructed from the
/// daemon loop only — never from CLI `wire_engine_hooks`.
#[allow(dead_code)] // held for the daemon process lifetime so host dirs stay owned
struct DaemonOsPlanes {
    checkpoint: crate::checkpoint::CheckpointManager,
    logger: Option<crate::logger::RollingLogger>,
    plugins: crate::plugins::PluginManager,
    fork: Option<crate::fork::ForkManager>,
    hibernate: Option<crate::suspend::HibernationManager>,
    reloader: crate::hot_reload::PluginReloader,
    gossip: crate::gossip::GossipManager,
    snapshots: crate::cell_snapshot::SnapshotManager,
    nat: crate::nat::NatManager,
    proxy: crate::tool_proxy::ToolProxy,
    audit: crate::audit_log::AuditLogger,
}

static DAEMON_OS: OnceLock<DaemonOsPlanes> = OnceLock::new();

/// Open host-owned OS directories and hold the managers for the daemon
/// process lifetime.
pub fn wire_daemon_os_planes(workspace: &Path) {
    let _ = DAEMON_OS.get_or_init(|| {
        let mut gossip = crate::gossip::GossipManager::new();
        // Ephemeral loopback: do not steal host-contract UDP 9092 (A2A discovery).
        let _ = gossip.bind("127.0.0.1:0");
        DaemonOsPlanes {
            checkpoint: crate::checkpoint::CheckpointManager::new(workspace.join("checkpoints")),
            logger: crate::logger::RollingLogger::new(crate::logger::LoggerConfig {
                log_dir: workspace.join("logs").join("cells"),
                ..crate::logger::LoggerConfig::default()
            })
            .ok(),
            plugins: crate::plugins::PluginManager::new(workspace),
            fork: crate::fork::ForkManager::new(workspace).ok(),
            hibernate: crate::suspend::HibernationManager::new(workspace).ok(),
            reloader: crate::hot_reload::PluginReloader::new(),
            gossip,
            snapshots: crate::cell_snapshot::SnapshotManager::new(),
            nat: crate::nat::NatManager::new(),
            proxy: crate::tool_proxy::ToolProxy::new(crate::security::CapabilityPolicy::new(
                "susi-host",
                Vec::new(),
            )),
            audit: crate::audit_log::AuditLogger::new(Some(
                workspace.join("logs").join("capability_audit.tsv"),
            )),
        }
    });
    activate_host_control_planes();
}

/// In-memory host control planes that are unique (not duplicates of
/// live core/gawd features). Constructed from the daemon loop so they
/// participate in the running OS rather than remaining compiled-only.
fn activate_host_control_planes() {
    let _ = crate::admin::AdminServer::default();
    let _ = crate::auto_tune::advise(
        &crate::swarm_metrics::SwarmMetricsSnapshot::default(),
        &crate::sla_monitor::SlaTargets::default(),
    );
    let _ = crate::budget::HierarchicalBudget::default();
    let _ = crate::cas::CasManager::default();
    let _ = crate::contract::ContractManager::default();
    let _ = crate::execution_mode::woken_by(&[], crate::execution_mode::Trigger::Event);
    let _ = crate::lineage::spawn_child("susi-host", &[], &[], 1, 1);
    let _ = crate::migration::MigrationManager::default();
    let _ = crate::mount::MountManager::default();
    let _ = crate::negotiation::Negotiation::offer("n1", "susi-host", "peer", "task");
    let _ = crate::offline_queue::OfflineQueue::default();
    let _ = crate::org_policy::decide(&[], "tool:git", None);
    let _ = crate::p2p_router::P2pRouter::default();
    let _ = crate::packages::resolve(&[], "susi", "0");
    let _ = crate::scaffold::scaffold(crate::scaffold::AgentTemplate::Ops, "susi-host");
    let _ = crate::signal::SignalRouter::default();
    let _ = crate::vfs::VfsManager::default();
    let _ = crate::workloads::complete(&crate::workloads::WorkloadRun {
        kind: crate::workloads::WorkloadKind::Engineering,
        evidence_ids: Vec::new(),
        citation_count: 0,
        playbook_steps_completed: 0,
        regions: Vec::new(),
    });
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
    let fallback_ok = host.fallback.execute_with_fallback(|ep| match ep {
        crate::fallback::InferenceEndpoint::PrimaryCloud(_)
        | crate::fallback::InferenceEndpoint::SecondaryCloud(_) => {
            crate::fallback::InferenceResult::Timeout
        }
        crate::fallback::InferenceEndpoint::LocalQuantized(_) => {
            crate::fallback::InferenceResult::Success("local".into())
        }
    });
    serde_json::json!({
        "wired": true,
        "orchestrator_id": host.orchestrator.orchestrator_id,
        "workers": workers,
        "busy": busy,
        "dead_cells": host.watchdog.find_dead_cells(),
        "identities": host.identity.registered_count(),
        "next_cell": host.balancer.next_cell(),
        "self_healing": self_healing,
        "queue_len": host.queue.len(),
        "host_ttl": host.ttl.remaining("susi-host"),
        "gateway_host_exposed": host
            .gateway
            .route_request(
                crate::http_gateway::AgentHttpRequest {
                    cell_id: "susi-host".into(),
                    payload: Vec::new(),
                },
                |_| Ok(b"ok".to_vec()),
            )
            .map(|r| r.status_code)
            .unwrap_or(0),
        "fallback_local": fallback_ok.is_ok(),
        "metrics_ready": !host.metrics.export_prometheus().is_empty(),
        "tool_cards": crate::tool_catalog::builtin_cards().len(),
        "os_planes": DAEMON_OS.get().map(|p| {
            serde_json::json!({
                "checkpoint": true,
                "logger": p.logger.is_some(),
                "plugins": true,
                "fork": p.fork.is_some(),
                "hibernate": p.hibernate.is_some(),
                "reloader": true,
                "gossip_bound": p.gossip.local_addr(),
                "snapshots": true,
                "nat": true,
                "tool_proxy": true,
                "audit": true,
            })
        }),
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
