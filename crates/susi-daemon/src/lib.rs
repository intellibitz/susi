#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub mod admin;
pub mod ambient;
pub mod audit_log;
pub mod auto_discovery;
pub mod auto_tune;
pub mod blackboard;
pub mod budget;
pub mod cas;
pub mod cell_snapshot;
pub mod cell_watcher;
pub mod checkpoint;
pub mod code_signing;
pub mod composition;
pub mod composition_recommender;
pub mod context_adapters;
pub mod dashboard;
pub mod discovery_pipeline;
pub mod docs_generator;
pub mod elastic_scheduler;
pub mod engine_hooks;
pub mod engine_watchdog;
pub mod event_log;
pub mod fork;
pub mod gmcp_bootstrap;
pub mod gossip;
pub mod hot_reload;
pub mod hot_reload_coverage;
pub mod identity;
pub mod inspector;
pub mod leaderboard;
pub mod load_balancer;
pub mod logger;
pub mod metrics;
pub mod nat;
pub mod orchestrator;
pub mod p2p_router;
pub mod plugins;
pub mod privacy;
pub mod resource_governor;
pub mod runtime_admin;
pub mod scheduled_missions;
pub mod security;
pub mod server;
pub mod service_supervision;
pub mod signal;
pub mod sla_monitor;
pub mod supervisor;
pub mod suspend;
pub mod swarm_metrics;
pub mod task_queue;
pub mod telemetry;
pub mod tls;
pub mod tool_catalog;
pub mod tool_proxy;
pub mod topology;
pub mod tracing;
pub mod ttl;
pub mod watchdog;
pub mod webhook_dispatcher;
pub mod what_if;
pub mod workflows;
pub mod zc_daemon_lifecycle;
pub mod zc_keys_health_boot;
pub mod zc_port_autoselect;
pub mod zc_rediscovery_adaptive;
pub mod zc_scorecard;
pub mod zc_startup_autofix;

#[cfg(test)]
#[path = "tests/vc_202_010_mastery.rs"]
mod vc_202_010_mastery_tests;

pub use composition::{swarm_host_snapshot, wire_cli_substrate, wire_engine_hooks};
pub use engine_hooks::SusiEngineHooks;
pub use server::SusiDaemon;
pub use service_supervision::{RestartEvent, ServiceStatus, Supervisor};
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
pub mod eco_drift_alerts;
pub mod eco_probe_scheduler;
