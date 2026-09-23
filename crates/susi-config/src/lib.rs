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

//! SUSI configuration substrate: `SusiConfig` dynamic registry, typed config
//! fragments, and shared self-healing JSON load/merge/save helpers.
//!
//! Layered below `susi-sandbox` (which re-exports this surface through
//! `susi_sandbox::manager` for back-compat) — this crate may only depend on
//! `susi-error` and `susi-paths`, never on feature crates above it.

pub mod cluster_key;
mod config;
pub mod extensions;
mod json_util;
mod types;
pub mod versioned_store;

pub use config::SusiConfig;
pub use json_util::{
    atomic_write_json_pretty, confined_workspace_join, http_agent, merge_missing_json_defaults,
    merge_missing_registry_defaults, DynamicRegistry, DynamicValue, ModelTier, ProviderType,
    StringRegistry,
};
pub use types::*;
pub use versioned_store::VersionedJsonStore;

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
