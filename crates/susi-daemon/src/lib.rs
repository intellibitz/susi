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

pub mod ambient;
pub mod auto_discovery;
pub mod blackboard;
pub mod composition;
pub mod context_adapters;
pub mod engine_hooks;
pub mod gmcp_bootstrap;
pub mod privacy;
pub mod runtime_admin;
pub mod server;
pub mod supervisor;
pub mod telemetry;
pub mod tls;
pub mod sandbox_wasm;
pub mod security;
pub mod cell_watcher;
pub mod scheduler;
pub mod workflows;
pub mod event_log;
pub mod suspend;
pub mod semantic_memory;
pub mod self_healing;
pub mod tool_proxy;
pub mod topology;
pub mod identity;
pub mod fork;
pub mod scratchfs;
pub mod metrics;
pub mod gossip;
pub mod plugins;
pub mod schema;
pub mod rate_limit;
pub mod vfs;
pub mod logger;
pub mod watchdog;
pub mod cas;
pub mod contract;
pub mod replay;
pub mod ring_buffer;
pub mod admin;
pub mod memory_quota;
pub mod fallback;

pub use composition::{wire_cli_substrate, wire_engine_hooks};
pub use server::SusiDaemon;
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
