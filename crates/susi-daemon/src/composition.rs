//! Composition roots: the only places that assemble concrete adapters.
//!
//! - CLI: [`wire_cli_substrate`]
//! - Daemon: [`wire_engine_hooks`] then [`crate::discovery_pipeline::bootstrap_zero_config_substrate`]
//!
//! See `ARCHITECTURE.md` at the repo root.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

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
static HOST_PLANES: OnceLock<HostControlPlanes> = OnceLock::new();
static OS_TICK_STARTED: OnceLock<()> = OnceLock::new();

struct HostControlPlanes {
    admin: crate::admin::AdminServer,
    budget: crate::budget::HierarchicalBudget,
    cas: crate::cas::CasManager,
    p2p: crate::p2p_router::P2pRouter,
    signal: crate::signal::SignalRouter,
    policy: crate::security::CapabilityPolicy,
    cloud: Mutex<Vec<susi_vendor_cloud::CloudInventory>>,
    ticks: AtomicU64,
}

fn gossip_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.gossip_port())
        .unwrap_or(susi_paths::ports::effective(susi_paths::ports::GOSSIP))
}

fn os_planes_report_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::substrate_home().join("os_planes.json")
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn bind_swarm_gossip(gossip: &mut crate::gossip::GossipManager) {
    let port = gossip_port();
    let bind = format!("0.0.0.0:{port}");
    if gossip.bind(&bind).is_err() {
        let _ = gossip.bind("0.0.0.0:0");
    }
}

/// Open host-owned OS directories and hold the managers for the daemon
/// process lifetime. Gossip binds instance UDP 9095 (not A2A 9092).
pub fn wire_daemon_os_planes(workspace: &Path) {
    let _ = DAEMON_OS.get_or_init(|| {
        let mut gossip =
            crate::gossip::GossipManager::with_store(workspace.join("gossip_caps.json"));
        bind_swarm_gossip(&mut gossip);
        gossip.spawn_recv_loop();
        let audit = crate::audit_log::AuditLogger::new(Some(
            workspace.join("logs").join("capability_audit.tsv"),
        ));
        let _ = audit.log_grant("susi-host", "root");
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
            nat: {
                let nat = crate::nat::NatManager::new();
                if let Ok(ip) = std::env::var("SUSI_PUBLIC_IP") {
                    let ip = ip.trim();
                    if !ip.is_empty() {
                        nat.perform_discovery(ip, crate::nat::NatStatus::Open);
                    }
                }
                nat
            },
            proxy: crate::tool_proxy::ToolProxy::new(crate::security::CapabilityPolicy::new(
                "susi-host",
                Vec::new(),
            )),
            audit,
        }
    });
    activate_host_control_planes(workspace);
    spawn_nat_discovery();
    start_os_plane_ticks();
}

fn spawn_nat_discovery() {
    let _ = std::thread::Builder::new()
        .name("susi-stun".into())
        .spawn(|| {
            if let Some(planes) = DAEMON_OS.get() {
                match planes.nat.status() {
                    crate::nat::NatStatus::Unknown => match planes.nat.discover_default() {
                        Ok(crate::nat::NatStatus::Symmetric) => {
                            let _ = planes.nat.allocate_turn_from_env();
                        }
                        Ok(_) => {}
                        Err(_) => {
                            let _ = planes.nat.allocate_turn_from_env();
                        }
                    },
                    crate::nat::NatStatus::Symmetric => {
                        let _ = planes.nat.allocate_turn_from_env();
                    }
                    crate::nat::NatStatus::Open
                    | crate::nat::NatStatus::PortRestricted
                    | crate::nat::NatStatus::TurnRelayed => {}
                }
                persist_os_planes_report();
            }
        });
}

/// Live host control planes held for the daemon lifetime and driven on a tick.
fn activate_host_control_planes(_workspace: &Path) {
    let _ = HOST_PLANES.get_or_init(|| {
        let budget = crate::budget::HierarchicalBudget::default();
        budget.set_cell_cap("susi-host", 1_000_000);
        let policy = crate::security::CapabilityPolicy::new(
            "susi-host",
            vec![
                crate::security::CapabilityGrant {
                    capability: "root".to_string(),
                    scope: None,
                    ephemeral: false,
                },
                crate::security::CapabilityGrant {
                    capability: "infer".to_string(),
                    scope: None,
                    ephemeral: false,
                },
                crate::security::CapabilityGrant {
                    capability: "tool:*".to_string(),
                    scope: None,
                    ephemeral: false,
                },
            ],
        );
        let signal = crate::signal::SignalRouter::default();
        let _ = signal.dispatch_signal("susi-host", crate::signal::CellSignal::SigCont);
        HostControlPlanes {
            admin: crate::admin::AdminServer::default(),
            budget,
            cas: crate::cas::CasManager::default(),
            p2p: crate::p2p_router::P2pRouter::default(),
            signal,
            policy,
            cloud: Mutex::new(susi_vendor_cloud::probe_all()),
            ticks: AtomicU64::new(0),
        }
    });
}

fn start_os_plane_ticks() {
    let _ = OS_TICK_STARTED.get_or_init(|| {
        tick_host_control_planes();
        let _ = std::thread::Builder::new()
            .name("susi-os-tick".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_secs(30));
                    tick_host_control_planes();
                }
            });
    });
}

fn tick_host_control_planes() {
    let Some(host) = HOST_PLANES.get() else {
        return;
    };
    let Some(os) = DAEMON_OS.get() else {
        return;
    };
    let n = host.ticks.fetch_add(1, Ordering::Relaxed);
    let _ = host.admin.get_diagnostics(&host.policy, 1, 0);
    let _ = host.budget.try_spend("susi", "susi", "susi-host", 0);
    let _ = host.cas.put(b"susi-host-heartbeat".to_vec());
    let _ = crate::auto_discovery::reap_exited_cells();
    let _ = os.plugins.list_plugins();
    let _ = host.signal.run_state("susi-host");
    if let Some(log) = &os.logger {
        let _ = log.log("susi-host", "info", &format!("os tick {n}"));
    }
    let _ = os
        .checkpoint
        .save_checkpoint(&crate::checkpoint::CellCheckpoint {
            cell_id: "susi-host".into(),
            timestamp: unix_secs(),
            memory_state: Vec::new(),
            step_counter: n,
        });
    os.snapshots
        .set_base_snapshot("susi-host", n.to_le_bytes().to_vec());
    let port = gossip_port();
    os.gossip.fanout_to_cluster(port);
    if let Ok(peers) = susi_core::plane_bus::gawd::cluster_peers() {
        for (id, addr) in peers {
            host.p2p.add_peer(&id, &addr);
        }
    }
    if let Ok(ma) = os
        .nat
        .generate_external_multiaddr(susi_paths::ports::effective(susi_paths::ports::A2A_HTTP))
    {
        host.p2p.add_peer("susi-host", &ma);
    }
    {
        let mut cloud = host.cloud.lock().unwrap_or_else(|e| e.into_inner());
        *cloud = susi_vendor_cloud::probe_all();
    }
    persist_os_planes_report();
}

fn nat_status_label(status: crate::nat::NatStatus) -> &'static str {
    match status {
        crate::nat::NatStatus::Open => "open",
        crate::nat::NatStatus::Symmetric => "symmetric",
        crate::nat::NatStatus::PortRestricted => "port_restricted",
        crate::nat::NatStatus::TurnRelayed => "turn_relayed",
        crate::nat::NatStatus::Unknown => "unknown",
    }
}

fn os_planes_json() -> serde_json::Value {
    let os = DAEMON_OS.get();
    let host = HOST_PLANES.get();
    serde_json::json!({
        "checkpoint": os.is_some(),
        "logger": os.is_some_and(|p| p.logger.is_some()),
        "plugins": os.is_some(),
        "fork": os.is_some_and(|p| p.fork.is_some()),
        "hibernate": os.is_some_and(|p| p.hibernate.is_some()),
        "reloader": os.is_some(),
        "gossip_bound": os.and_then(|p| p.gossip.local_addr()),
        "gossip_port": gossip_port(),
        "gossip_auth": crate::gossip::GossipManager::auth_mode(),
        "gossip_peers": os.map(|p| p.gossip.peer_count()),
        "gossip_rejected": os.map(|p| p.gossip.rejected_count()),
        "snapshots": os.is_some(),
        "nat_status": os.map(|p| nat_status_label(p.nat.status())),
        "nat_public_ip": os.and_then(|p| p.nat.public_ip()),
        "nat_stun_error": os.and_then(|p| p.nat.last_error()),
        "nat_turn_relay": os.and_then(|p| p.nat.turn_relay()),
        "nat_multiaddr": os.and_then(|p| {
            p.nat
                .generate_external_multiaddr(susi_paths::ports::effective(
                    susi_paths::ports::A2A_HTTP,
                ))
                .ok()
        }),
        "tool_proxy": os.is_some(),
        "audit": os.is_some(),
        "driven": host.is_some(),
        "ticks": host.map(|h| h.ticks.load(Ordering::Relaxed)),
        "budget_remaining": host.and_then(|h| h.budget.cell_remaining("susi-host")),
        "host_run_state": host.and_then(|h| {
            h.signal.run_state("susi-host").map(|s| match s {
                crate::signal::CellRunState::Running => "running",
                crate::signal::CellRunState::Stopped => "stopped",
                crate::signal::CellRunState::Terminated => "terminated",
            })
        }),
        "admin_uptime_secs": host.and_then(|h| {
            h.admin
                .get_diagnostics(&h.policy, 1, 0)
                .ok()
                .map(|d| d.uptime_seconds)
        }),
        "cloud": host.and_then(|h| {
            h.cloud.lock().ok().map(|inv| {
                inv.iter()
                    .map(|c| {
                        serde_json::json!({
                            "kind": c.kind,
                            "tool": c.tool,
                            "available": c.available,
                            "summary": c.summary,
                        })
                    })
                    .collect::<Vec<_>>()
            })
        }),
    })
}

fn persist_os_planes_report() {
    if let Ok(text) = serde_json::to_string_pretty(&os_planes_json()) {
        let _ = std::fs::write(os_planes_report_path(), text);
    }
}

/// CLI-readable OS-plane report written by the daemon tick (OnceLocks are
/// process-local and invisible to `susi os`).
pub fn load_os_planes_report() -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(os_planes_report_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Snapshot of the in-process swarm host for `susi os`. Wires the host
/// if the daemon/CLI composition root has not yet.
pub fn swarm_host_snapshot() -> serde_json::Value {
    wire_swarm_host();
    let Some(host) = SWARM_HOST.get() else {
        return serde_json::json!({ "wired": false });
    };
    let (workers, busy) = host.orchestrator.pool_summary();
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
        "os_planes": if DAEMON_OS.get().is_some() {
            os_planes_json()
        } else {
            load_os_planes_report().unwrap_or(serde_json::Value::Null)
        },
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
    susi_gemi::engines::http_provider::register_configured_cloud_endpoints(
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
