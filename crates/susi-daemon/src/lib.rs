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

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_sandbox;

pub mod ab_swarm;
pub mod admin;
pub mod ambient;
pub mod approval;
pub mod audit_log;
pub mod auth;
pub mod auto_discovery;
pub mod auto_tune;
pub mod bi_export;
pub mod blackboard;
pub mod budget;
pub mod cas;
pub mod causal_ledger;
pub mod cell_profile;
pub mod cell_snapshot;
pub mod cell_watcher;
pub mod change_gate;
pub mod checkpoint;
pub mod code_signing;
pub mod compliance_map;
pub mod composition;
pub mod composition_recommender;
pub mod consensus;
pub mod context_adapters;
pub mod contract;
pub mod cost_analyzer;
pub mod cron;
pub mod dashboard;
pub mod db_index;
pub mod dead_letter;
pub mod discovery;
pub mod docs_generator;
pub mod elastic_scheduler;
pub mod encryption;
pub mod engine_hooks;
pub mod event_log;
pub mod event_sourcing;
pub mod execution_mode;
pub mod failover;
pub mod fallback;
pub mod fault_injection;
pub mod fork;
pub mod gc;
pub mod gmcp_bootstrap;
pub mod gossip;
pub mod gpu_scheduler;
pub mod green_mode;
pub mod hot_reload;
pub mod http_gateway;
pub mod identity;
pub mod inspector;
pub mod leaderboard;
pub mod lineage;
pub mod load_balancer;
pub mod logger;
pub mod marketplace;
pub mod memory_quota;
pub mod metrics;
pub mod metrics_aggregator;
pub mod metrics_export;
pub mod migration;
pub mod mount;
pub mod nat;
pub mod negotiation;
pub mod offline_queue;
pub mod orchestrator;
pub mod org_policy;
pub mod p2p_router;
pub mod packages;
pub mod playbook;
pub mod playground;
pub mod plugins;
pub mod privacy;
pub mod pubsub;
pub mod query_api;
pub mod rate_limit;
pub mod reference_arch;
pub mod registry;
pub mod replay;
pub mod residency;
pub mod ring_buffer;
pub mod root_cause;
pub mod runtime_admin;
pub mod sandbox_network;
pub mod sandbox_wasm;
pub mod scaffold;
pub mod scheduler;
pub mod schema;
pub mod scratchfs;
pub mod secret_store;
pub mod security;
pub mod security_scan;
pub mod self_healing;
pub mod semantic_memory;
pub mod server;
pub mod signal;
pub mod simulation;
pub mod sla_monitor;
pub mod supervisor;
pub mod suspend;
pub mod swarm_metrics;
pub mod task_queue;
pub mod telemetry;
pub mod telemetry_stream;
pub mod tls;
pub mod tool_catalog;
pub mod tool_proxy;
pub mod topology;
pub mod tracing;
pub mod ttl;
pub mod vfs;
pub mod wasm_gas;
pub mod watchdog;
pub mod webhook_dispatcher;
pub mod what_if;
pub mod workflows;
pub mod workloads;
pub mod world_model;

pub use composition::{wire_cli_substrate, wire_engine_hooks};
pub use server::SusiDaemon;
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
