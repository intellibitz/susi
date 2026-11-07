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
        let missing = std::env::temp_dir().join("nonexistent-susi-wasm-1104.wasm");
        assert!(wasm::WasmHost::execute_reflex(&missing, "").is_err());
        assert!(wasm::WasmHost::execute_untrusted_wasm(&missing, "arg").is_err());
    }

    /// Compile a readable fixture to the binary module the host accepts.
    fn module(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("test fixture compiles")
    }

    #[test]
    fn wasm_host_executes_a_binary_module_and_rejects_uncompiled_input() {
        let dir = std::env::temp_dir().join(format!("susi-wasm-lib-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hero = dir.join("minimal.wasm");
        std::fs::write(&hero, module(r#"(module (func (export "_start")))"#)).unwrap();
        assert_eq!(
            wasm::WasmHost::execute_reflex(&hero, "").unwrap(),
            "Wasm execution success (No Output)"
        );

        // Text that was never compiled is refused by the host: the compile
        // step is the producer's, not the socket's.
        let text = dir.join("uncompiled.wat");
        std::fs::write(&text, r#"(module (func (export "_start")))"#).unwrap();
        let error = wasm::WasmHost::execute_reflex(&text, "")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("binary WebAssembly module"),
            "unexpected error: {error}"
        );

        let bad = dir.join("bad.wasm");
        std::fs::write(&bad, "not valid wat or wasm").unwrap();
        assert!(wasm::WasmHost::execute_reflex(&bad, "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wasm_cell_from_bytes_rejects_empty_and_invalid_wasm_payloads() {
        assert!(cell::WasmCell::from_bytes(b"").is_err());
        assert!(cell::WasmCell::from_bytes(b"not-valid-wat").is_err());
    }
}
