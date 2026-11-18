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
pub mod disk_space;
mod json_util;
mod nonce;
pub mod plain_http;
pub mod process_probe;
pub mod progress;
pub mod redaction;
pub use redaction::{redact_credentials, GovernancePatterns};
pub mod vault_exec;
#[cfg(windows)]
pub mod windows_model_identity;
pub use nonce::random_nonce_hex;

pub use json_util::{
    atomic_replace_unsynced, atomic_write_bytes, merge_missing_json_defaults, read_atomic_bytes,
    DynamicRegistry, DynamicValue, JsonFields,
};

/// Marks this process as the `susi-config` service (served by
/// `susi-leaf-services`): it is the canonical writer, so config reads and
/// writes resolve locally instead of calling back into the service.
pub fn enter_service_mode() {
    susi_paths::enter_service_mode();
}
