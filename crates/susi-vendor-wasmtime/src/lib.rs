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

//! Rust-only WebAssembly cell and WASI reflex host.
//!
//! The historical facade name remains stable for consumers. Execution uses
//! Wasmi, with bounded fuel/memory/output and an explicit capability-limited
//! WASI implementation; no Wasmtime, native JIT, WASI-common or wiggle runtime
//! is linked. The native service's HTTP shell remains in susi-leaf-services.

pub use susi_error;
pub mod cell;
mod wasi;
pub mod wasm;

#[cfg(test)]
mod tests {
    #[test]
    fn locked_runtime_is_the_rust_interpreter_not_the_native_jit() {
        let lock = include_str!("../../../Cargo.lock");
        assert!(lock.contains("name = \"wasmi\"\n"));
        assert!(!lock.contains("name = \"wasmtime\"\n"));
        assert!(!lock.contains("name = \"wasmtime-wasi\"\n"));
    }
}
