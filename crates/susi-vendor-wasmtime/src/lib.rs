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

//! # susi-vendor-wasmtime
//!
//! The one crate that links wasmtime: WASI reflex execution (`wasm`) for the
//! `susi-native` leaf service, and the metered in-process cell runtime
//! (`cell`) the daemon's auto-discovery and plugin hot-reload use. The
//! service's HTTP shell lives in `susi-leaf-services` (default bind
//! `127.0.0.1:18084`, `SUSI_NATIVE_PORT`). Feature planes never link this
//! crate: they reach the service through `susi-native-client`.
//!
//! wasmtime replaced wasmer as the vendored engine because wasmer 7.4.2 has
//! no compliant native backend on Windows — singlepass and cranelift both
//! `compile_error!()` under `cfg(windows)`, singlepass is BUSL-1.1 for
//! production use, and the v8 backend exposes no middleware API for fuel
//! metering. wasmtime meters fuel natively, so `CELL_FUEL` semantics carry
//! over unchanged.

pub use susi_error;

pub mod cell;
pub mod wasm;
