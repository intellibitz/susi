//! WASM sandbox support for SUSI Swarm Cells.
//!
//! This module provides a minimal runtime for loading and executing
//! Rust‑compiled WebAssembly modules (cells) inside the SUSI daemon.
//! It uses the `wasmtime` crate with the Cranelift codegen backend.
//!
//! The design follows the Phase 1 goal of **WASM + namespace isolation**
//! while keeping the API simple for later extensions (e.g., capability
//! enforcement, resource limits).
//!
//! Host ABI: a cell may import `host_send(ptr: i32, len: i32) -> i32` from
//! the `env` or `susi` module. The host copies `len` bytes at `ptr` out of
//! the cell's exported `memory` into a bounded outbox (drained with
//! [`WasmCell::take_messages`]) and returns `0`, or a negative code when the
//! message is rejected. Imports are resolved by name, so cells that import
//! nothing instantiate too.

use anyhow::{Context, Result};
use std::path::PathBuf;
use wasmtime::{
    Caller, Engine, Extern, Instance, Linker, Module, Store, StoreLimits, StoreLimitsBuilder,
};

/// Largest single `host_send` payload the host accepts.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Most undrained messages a cell may queue before sends are rejected.
pub const MAX_OUTBOX_MESSAGES: usize = 1024;

/// `host_send` return codes.
const SEND_OK: i32 = 0;
const SEND_BAD_ARGS: i32 = -1;
const SEND_NO_MEMORY: i32 = -2;
const SEND_OUT_OF_BOUNDS: i32 = -3;
const SEND_OUTBOX_FULL: i32 = -4;

/// Per-cell host state held in the Wasmtime store.
#[derive(Default)]
struct HostState {
    outbox: Vec<Vec<u8>>,
    limits: StoreLimits,
}

/// Linear-memory cap per cell: a runaway cell traps instead of growing the
/// daemon's own memory without bound.
pub const MAX_CELL_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Represents a loaded WASM cell.
///
/// * `module_path` – path to the compiled `.wasm` file.
/// * `instance` – instantiated module ready for calls.
/// * `store` – the Wasmtime store holding the instance and host state.
pub struct WasmCell {
    module_path: PathBuf,
    instance: Instance,
    store: Store<HostState>,
}

/// Copy a guest message out of the caller's exported memory into the outbox.
fn host_send(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) -> i32 {
    let (Ok(start), Ok(len)) = (usize::try_from(ptr), usize::try_from(len)) else {
        return SEND_BAD_ARGS;
    };
    if len > MAX_MESSAGE_BYTES {
        return SEND_BAD_ARGS;
    }
    if caller.data().outbox.len() >= MAX_OUTBOX_MESSAGES {
        return SEND_OUTBOX_FULL;
    }
    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
        return SEND_NO_MEMORY;
    };
    let Some(end) = start.checked_add(len) else {
        return SEND_OUT_OF_BOUNDS;
    };
    let Some(bytes) = memory.data(&caller).get(start..end).map(<[u8]>::to_vec) else {
        return SEND_OUT_OF_BOUNDS;
    };
    tracing::info!("[WasmCell] host_send received {} byte(s)", bytes.len());
    caller.data_mut().outbox.push(bytes);
    SEND_OK
}

impl WasmCell {
    /// Load a WASM binary from `module_path` and instantiate it.
    ///
    /// # Errors
    /// Fails when the file cannot be read or compiled, or instantiation fails.
    pub fn load(module_path: impl Into<PathBuf>) -> Result<Self> {
        let module_path = module_path.into();
        let engine = Engine::default();
        let module = Module::from_file(&engine, &module_path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("Failed to load WASM module {}", module_path.display()))?;
        Self::instantiate(&engine, &module, module_path)
    }

    /// Instantiate a cell from in-memory WASM (binary or text format).
    ///
    /// # Errors
    /// Fails when the bytes do not compile or instantiation fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes)
            .map_err(anyhow::Error::from)
            .context("Failed to compile WASM module")?;
        Self::instantiate(&engine, &module, PathBuf::from("<memory>"))
    }

    fn instantiate(engine: &Engine, module: &Module, module_path: PathBuf) -> Result<Self> {
        let mut linker: Linker<HostState> = Linker::new(engine);
        for namespace in ["env", "susi"] {
            linker
                .func_wrap(namespace, "host_send", host_send)
                .map_err(anyhow::Error::from)
                .context("Failed to define host_send")?;
        }
        let mut store = Store::new(
            engine,
            HostState {
                outbox: Vec::new(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(MAX_CELL_MEMORY_BYTES)
                    .instances(1)
                    .build(),
            },
        );
        store.limiter(|state| &mut state.limits);
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(anyhow::Error::from)
            .context("Failed to instantiate WASM module")?;
        Ok(Self {
            module_path,
            instance,
            store,
        })
    }

    /// Path the cell was loaded from (`<memory>` for [`WasmCell::from_bytes`]).
    #[must_use]
    pub fn module_path(&self) -> &std::path::Path {
        &self.module_path
    }

    /// Drain the messages the cell has sent via `host_send`, oldest first.
    pub fn take_messages(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.store.data_mut().outbox)
    }

    /// Execute the `_start` function (or any exported function name).
    /// Returns the result of the function as a string for debugging.
    ///
    /// # Errors
    /// Fails when the export is missing, is not `() -> i32`, or traps.
    pub fn execute(&mut self, func_name: &str) -> Result<String> {
        let func = self
            .instance
            .get_func(&mut self.store, func_name)
            .with_context(|| format!("Function '{func_name}' not found in WASM module"))?;

        // Entry points take no params and return an i32 status.
        let typed = func
            .typed::<(), i32>(&self.store)
            .map_err(anyhow::Error::from)
            .with_context(|| "Failed to cast function signature")?;
        let ret = typed.call(&mut self.store, ())?;
        Ok(format!("WASM function '{func_name}' returned {ret}"))
    }
}

/// Spawn a WASM cell from a `.wasm` file at the given path.
///
/// The cell is loaded and instantiated but not yet executed — the caller
/// should invoke [`WasmCell::execute`] with the desired entry-point
/// (typically `"_start"`).
///
/// # Errors
/// See [`WasmCell::load`].
pub fn spawn_wasm_cell(wasm_path: PathBuf) -> Result<WasmCell> {
    WasmCell::load(wasm_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER: &str = r#"(module
        (import "env" "host_send" (func $send (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 16) "hello host")
        (func (export "_start") (result i32)
            (call $send (i32.const 16) (i32.const 10))))"#;

    #[test]
    fn host_send_copies_guest_payload_into_outbox() {
        let mut cell = WasmCell::from_bytes(SENDER.as_bytes()).unwrap();
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned 0"
        );
        assert_eq!(cell.take_messages(), vec![b"hello host".to_vec()]);
        assert!(cell.take_messages().is_empty(), "outbox drains");
    }

    #[test]
    fn host_send_rejects_out_of_bounds_reads() {
        let wat = r#"(module
            (import "susi" "host_send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32)
                (call $send (i32.const 65530) (i32.const 100))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -3"));
        assert!(cell.take_messages().is_empty());
    }

    #[test]
    fn host_send_without_exported_memory_is_rejected() {
        let wat = r#"(module
            (import "env" "host_send" (func $send (param i32 i32) (result i32)))
            (func (export "_start") (result i32)
                (call $send (i32.const 0) (i32.const 1))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -2"));
    }

    #[test]
    fn memory_growth_beyond_the_cap_is_refused() {
        // 1 page initially; memory.grow by 8192 pages (512 MiB) must fail
        // (returns -1) instead of growing the daemon's memory.
        let wat = r#"(module
            (memory (export "memory") 1)
            (func (export "_start") (result i32)
                (memory.grow (i32.const 8192))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn cells_without_imports_instantiate() {
        let wat = r#"(module (func (export "_start") (result i32) (i32.const 7)))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned 7"
        );
    }
}
