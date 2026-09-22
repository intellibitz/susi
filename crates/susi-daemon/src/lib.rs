pub mod ambient;
pub mod auto_discovery;
pub mod composition;
pub mod context_adapters;
pub mod engine_hooks;
pub mod privacy;
pub mod runtime_admin;
pub mod server;
pub mod telemetry;

pub use composition::{wire_cli_substrate, wire_engine_hooks};
pub use engine_hooks::SusiEngineHooks;
pub use server::SusiDaemon;
// SusiAdmin and EvolutionManager are used via fully qualified names in tools.rs
