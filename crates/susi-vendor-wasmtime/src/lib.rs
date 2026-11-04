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
//! is linked. Each store permits one instance, one 256 MiB linear memory and
//! one table of at most 1,000,000 entries. Module input is capped at 64 MiB;
//! WASI output and queued cell payloads are capped at 16 MiB. These are resource
//! bounds, not a claim that total interpreter RSS equals the linear-memory cap.
//! The native service's HTTP shell remains in susi-leaf-services.

pub use susi_error;
pub mod cell;
mod resources;
mod wasi;
pub mod wasm;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_runtime_is_the_rust_interpreter_not_the_native_jit() {
        let lock = include_str!("../../../Cargo.lock");
        assert!(lock.contains("name = \"wasmi\"\n"));
        assert!(!lock.contains("name = \"wasmtime\"\n"));
        assert!(!lock.contains("name = \"wasmtime-wasi\"\n"));
    }

    #[test]
    fn cell_budget_constants_and_error_reexport_match_documented_caps() {
        assert_eq!(cell::MAX_CELL_MEMORY_BYTES, 256 * 1024 * 1024);
        assert_eq!(cell::MAX_MESSAGE_BYTES, 1024 * 1024);
        assert_eq!(cell::MAX_OUTBOX_MESSAGES, 1024);
        assert_eq!(cell::CELL_FUEL, 10_000_000);
        let err = susi_error::EaiError::sandbox("cell boundary check");
        assert!(err.to_string().contains("cell boundary check"));
    }

    #[test]
    fn wasm_host_rejects_missing_module_path() {
        let missing = std::env::temp_dir().join("nonexistent-susi-wasm-1104.wat");
        assert!(wasm::WasmHost::execute_reflex(&missing, "").is_err());
        assert!(wasm::WasmHost::execute_untrusted_wasm(&missing, "arg").is_err());
    }

    #[test]
    fn wasm_host_executes_minimal_wat_start_and_rejects_malformed_wat() {
        let dir = std::env::temp_dir().join(format!("susi-wasm-lib-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let valid_wat = dir.join("minimal.wat");
        std::fs::write(&valid_wat, r#"(module (func (export "_start")))"#).unwrap();
        assert_eq!(
            wasm::WasmHost::execute_reflex(&valid_wat, "").unwrap(),
            "Wasm execution success (No Output)"
        );

        let bad_wat = dir.join("bad.wat");
        std::fs::write(&bad_wat, "not valid wat or wasm").unwrap();
        assert!(wasm::WasmHost::execute_reflex(&bad_wat, "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wasm_cell_from_bytes_rejects_empty_and_invalid_wasm_payloads() {
        assert!(cell::WasmCell::from_bytes(b"").is_err());
        assert!(cell::WasmCell::from_bytes(b"not-valid-wat").is_err());
    }
}
