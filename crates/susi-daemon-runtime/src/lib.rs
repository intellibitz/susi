//! Host runtime of the Susi daemon that needs no GAWD, GEMI, GMCP or harness
//! crate: leaf-service supervision and watchdogs, NAT traversal, TLS, port
//! fallback, resource governance and the startup checks. `susi-daemon`
//! re-exports every module under its old path (`susi_daemon::supervisor`, ...).
//! Kept apart so this code compiles while the pillar crates do, instead of
//! after them on the serial tail of `cargo build -p susi`.
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

pub mod blackboard;
pub mod boot_trace;
pub mod context_adapters;
pub mod eco_drift_alerts;
pub mod eco_probe_scheduler;
pub mod engine_watchdog;
pub mod hot_reload_coverage;
pub mod nat;
pub mod orchestrator;
pub mod os_provision;
pub mod port_fallback;
pub mod resource_governor;
pub mod scheduled_missions;
pub mod security;
pub mod service_supervision;
pub mod sla_monitor;
pub mod supervisor;
pub mod swarm_metrics;
pub mod telemetry;
pub mod tls;
pub mod watchdog;
pub mod zc_daemon_lifecycle;
pub mod zc_keys_health_boot;
pub mod zc_rediscovery_adaptive;
pub mod zc_scorecard;
pub mod zc_startup_autofix;
