#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-native
//!
//! Leaf REST service for Wasmer/WASI reflex execution. Default bind:
//! `127.0.0.1:18084` (`SUSI_NATIVE_PORT`). Feature crates vendor a
//! byte-identical `susi_native` module and reach this process over a thin
//! HTTP IPC client; there is no local fallback — `wasmer`/`wasmer-wasix`
//! stay linked into the service binary only.

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

pub mod wasm;
