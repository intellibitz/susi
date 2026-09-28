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
//! Wasmer/WASI integration for the `susi-native` leaf service, whose HTTP
//! shell lives in `susi-leaf-services` (default bind `127.0.0.1:18084`,
//! `SUSI_NATIVE_PORT`). Feature crates reach it through
//! `susi-native-client`; there is no local fallback — `wasmer`/`wasmer-wasix`
//! stay linked into the service process only.

pub use susi_error;

pub mod wasm;
