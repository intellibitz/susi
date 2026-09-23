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

pub mod ambient;
pub mod auto_discovery;
pub mod composition;
pub mod context_adapters;
pub mod engine_hooks;
pub mod gmcp_bootstrap;
pub mod privacy;
pub mod runtime_admin;
pub mod server;
pub mod telemetry;

pub use composition::{wire_cli_substrate, wire_engine_hooks};
pub use engine_hooks::SusiEngineHooks;
pub use server::SusiDaemon;
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
