//! Sandbox manager: dynamic config registry and workspace helpers.
//!
//! Split for Mandate 3 (bloat) readability; public paths stay `susi_sandbox::manager::*`.

mod config;
mod json_util;
mod runtime;
mod types;

pub use config::SusiConfig;
pub use json_util::{
    atomic_write_json_pretty, confined_workspace_join, http_agent, merge_missing_json_defaults,
    merge_missing_registry_defaults, DynamicRegistry, DynamicValue, ModelTier, ProviderType,
    StringRegistry,
};
pub use runtime::{
    IntentBundleManager, LogLevel, SandboxManager, SusiAuditLogger, SusiBackupManager, SusiMemory,
};
pub use types::*;
