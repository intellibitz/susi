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
pub mod process_probe;
pub mod progress;
pub mod vault_exec;
pub use nonce::random_nonce_hex;

pub use json_util::{atomic_replace_unsynced, read_atomic_bytes, DynamicValue, JsonFields};
