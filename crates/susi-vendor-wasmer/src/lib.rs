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

//! # susi-vendor-wasmer
//!
//! The one crate that links wasmer: WASI reflex execution (`wasm`) for the
//! `susi-native` leaf service, and the metered in-process cell runtime
//! (`cell`) the daemon's auto-discovery and plugin hot-reload use. The
//! service's HTTP shell lives in `susi-leaf-services` (default bind
//! `127.0.0.1:18084`, `SUSI_NATIVE_PORT`). Feature planes never link this
//! crate: they reach the service through `susi-native-client`.

pub use susi_error;

pub mod cell;
pub mod wasm;
