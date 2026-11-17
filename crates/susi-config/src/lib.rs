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
//! Generic configuration wire, atomic storage, host inspection and runtime bootstrap.

pub use susi_error;
pub mod config_bridge;
pub mod disk_space;
pub mod host_metrics;
mod json_util;
pub mod mcp;
pub mod mcp_client;
pub mod native_path;
mod nonce;
pub mod plain_http;
pub mod process_probe;
pub mod progress;
pub mod redaction;
pub use redaction::{redact_credentials, GovernancePatterns};
pub mod protocol_json;
pub mod storage_guard;
pub mod vault_exec;
#[cfg(windows)]
pub mod windows_model_identity;
pub mod workflow_wire;
pub mod workload_process;
pub use nonce::random_nonce_hex;

pub use json_util::{
    atomic_replace_file, atomic_replace_unsynced, atomic_write_bytes, atomic_write_json_pretty,
    clear_json_override, confined_workspace_join, create_private_dir, file_node,
    install_private_file, load_or_create_secret, merge_missing_json_defaults,
    merge_missing_registry_defaults, read_atomic_bytes, remove_file_if_present,
    write_json_override, DynamicRegistry, DynamicValue, JsonFields, ModelTier, ProviderType,
    StringRegistry,
};

/// Serializes tests that mutate or read process-global environment-derived
/// paths (`HOME`, `XDG_CONFIG_HOME`, `SUSI_*`). Mutators must hold this lock
/// for the whole env-swap window; readers of `SusiDirs`-derived paths must
/// hold it while resolving so a swapped HOME cannot flip path selection
/// mid-test.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks this process as the `susi-config` service (served by
/// `susi-leaf-services`): it is the canonical writer, so config reads and
/// writes resolve locally instead of calling back into the service.
pub fn enter_service_mode() {
    susi_paths::enter_service_mode();
}
