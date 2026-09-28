//! WASM sandbox support for SUSI Swarm Cells.
//!
//! This module provides a minimal runtime for loading and executing
//! Rust‑compiled WebAssembly modules (cells) inside the SUSI daemon.
//! It uses the `wasmer` crate (Cranelift; Singlepass on Windows) — the same
//! engine `susi-native` runs WASI modules on.
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

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::Arc;
use wasmer::sys::wasmparser::{Parser, Payload};
use wasmer::sys::{CompilerConfig, EngineBuilder};
use wasmer::wasmparser::Operator;
use wasmer::{
    Engine, Function, FunctionEnv, FunctionEnvMut, Imports, Instance, Memory, Module, Store,
};
use wasmer_middlewares::Metering;

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

/// Per-cell host state reachable from `host_send`.
#[derive(Default)]
struct HostState {
    outbox: Vec<Vec<u8>>,
    /// The cell's exported `memory`, bound right after instantiation.
    memory: Option<Memory>,
}

/// Linear-memory cap per cell: a runaway cell traps instead of growing the
/// daemon's own memory without bound.
pub const MAX_CELL_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Instruction budget (metering points, one per operator) per cell: an
/// infinite loop traps instead of pinning a daemon thread forever.
pub const CELL_FUEL: u64 = 10_000_000_000;

/// Engine with metering compiled in; every instance starts with [`CELL_FUEL`].
fn cell_engine() -> Engine {
    let metering = Arc::new(Metering::new(CELL_FUEL, |_: &Operator| 1));
    #[cfg(not(windows))]
    let mut compiler = wasmer::sys::Cranelift::default();
    #[cfg(windows)]
    let mut compiler = wasmer::sys::Singlepass::default();
    compiler.push_middleware(metering);
    EngineBuilder::new(compiler).engine().into()
}

/// Clamp every defined memory's maximum to [`MAX_CELL_MEMORY_BYTES`].
///
/// Wasmer enforces a memory's declared maximum on `memory.grow`, so capping
/// the declaration caps the cell — without a custom `Tunables`, whose
/// required `unsafe fn`s this crate's `unsafe_code` policy reserves for FFI.
/// Sections other than the memory section are copied through byte-for-byte.
/// Imported memories need no clamp: the host provides none, so such a cell
/// fails to instantiate.
fn cap_memory(wasm: &[u8]) -> Result<Vec<u8>> {
    let mut out = wasm_encoder::Module::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.context("Failed to parse WASM module")?;
        if let Payload::Version {
            encoding: wasmer::sys::wasmparser::Encoding::Component,
            ..
        } = payload
        {
            bail!("WASM components are not supported as cells");
        }
        if let Payload::MemorySection(reader) = payload {
            out.section(&capped_memory_section(reader)?);
            continue;
        }
        // Every other section is copied through as raw bytes; payloads that
        // are not sections (header, code entries, end) carry nothing to copy.
        if let Some((id, range)) = payload.as_section() {
            let (Ok(start), Ok(end)) = (usize::try_from(range.start), usize::try_from(range.end))
            else {
                bail!("WASM section range does not fit in memory");
            };
            let data = wasm
                .get(start..end)
                .context("WASM section range out of bounds")?;
            out.section(&wasm_encoder::RawSection { id, data });
        }
    }
    Ok(out.finish())
}

/// Re-encode a memory section with each maximum clamped to the cell cap.
fn capped_memory_section(
    reader: wasmer::sys::wasmparser::MemorySectionReader<'_>,
) -> Result<wasm_encoder::MemorySection> {
    let mut section = wasm_encoder::MemorySection::new();
    for memory in reader {
        let memory = memory.context("Failed to read memory section")?;
        // Checked: this runs before wasmer validates the module, so a
        // malformed page-size exponent must error, not overflow the shift.
        let page_bytes = 1u64
            .checked_shl(memory.page_size_log2.unwrap_or(16))
            .context("invalid WASM memory page size")?;
        let cap_pages = MAX_CELL_MEMORY_BYTES as u64 / page_bytes;
        if memory.initial > cap_pages {
            bail!(
                "cell memory starts at {} page(s), above the {} MiB cap",
                memory.initial,
                MAX_CELL_MEMORY_BYTES / (1024 * 1024)
            );
        }
        section.memory(wasm_encoder::MemoryType {
            minimum: memory.initial,
            maximum: Some(memory.maximum.map_or(cap_pages, |m| m.min(cap_pages))),
            memory64: memory.memory64,
            shared: memory.shared,
            page_size_log2: memory.page_size_log2,
        });
    }
    Ok(section)
}

/// Represents a loaded WASM cell.
///
/// * `module_path` – path to the compiled `.wasm` file.
/// * `instance` – instantiated module ready for calls.
/// * `store` – the Wasmer store holding the instance.
/// * `env` – handle to the cell's [`HostState`] inside `store`.
pub struct WasmCell {
    module_path: PathBuf,
    instance: Instance,
    store: Store,
    env: FunctionEnv<HostState>,
}

/// Copy a guest message out of the cell's exported memory into the outbox.
fn host_send(mut env: FunctionEnvMut<'_, HostState>, ptr: i32, len: i32) -> i32 {
    let (Ok(start), Ok(len)) = (u64::try_from(ptr), usize::try_from(len)) else {
        return SEND_BAD_ARGS;
    };
    if len > MAX_MESSAGE_BYTES {
        return SEND_BAD_ARGS;
    }
    let (state, store) = env.data_and_store_mut();
    if state.outbox.len() >= MAX_OUTBOX_MESSAGES {
        return SEND_OUTBOX_FULL;
    }
    let Some(memory) = &state.memory else {
        return SEND_NO_MEMORY;
    };
    let mut bytes = vec![0u8; len];
    // `read` bounds-checks `start + len` (overflow included) against the
    // current memory size.
    if memory.view(&store).read(start, &mut bytes).is_err() {
        return SEND_OUT_OF_BOUNDS;
    }
    tracing::info!("[WasmCell] host_send received {} byte(s)", bytes.len());
    state.outbox.push(bytes);
    SEND_OK
}

impl WasmCell {
    /// Load a WASM binary from `module_path` and instantiate it.
    ///
    /// # Errors
    /// Fails when the file cannot be read or compiled, or instantiation fails.
    pub fn load(module_path: impl Into<PathBuf>) -> Result<Self> {
        let module_path = module_path.into();
        let bytes = std::fs::read(&module_path)
            .with_context(|| format!("Failed to load WASM module {}", module_path.display()))?;
        Self::instantiate(&bytes, module_path)
    }

    /// Instantiate a cell from in-memory WASM (binary or text format).
    ///
    /// # Errors
    /// Fails when the bytes do not compile or instantiation fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::instantiate(bytes, PathBuf::from("<memory>"))
    }

    fn instantiate(bytes: &[u8], module_path: PathBuf) -> Result<Self> {
        let wasm = wasmer::wat2wasm(bytes).context("Failed to parse WASM text")?;
        let wasm = cap_memory(&wasm)?;
        let mut store = Store::new(cell_engine());
        let module = Module::new(&store, &wasm)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("Failed to compile WASM module {}", module_path.display()))?;
        let env = FunctionEnv::new(&mut store, HostState::default());
        let send = Function::new_typed_with_env(&mut store, &env, host_send);
        let mut imports = Imports::new();
        for namespace in ["env", "susi"] {
            imports.define(namespace, "host_send", send.clone());
        }
        let instance = Instance::new(&mut store, &module, &imports)
            .map_err(anyhow::Error::from)
            .context("Failed to instantiate WASM module")?;
        // Bound after instantiation: a `start` function that calls
        // `host_send` sees no memory yet and gets SEND_NO_MEMORY.
        env.as_mut(&mut store).memory = instance.exports.get_memory("memory").ok().cloned();
        Ok(Self {
            module_path,
            instance,
            store,
            env,
        })
    }

    /// Path the cell was loaded from (`<memory>` for [`WasmCell::from_bytes`]).
    #[must_use]
    pub fn module_path(&self) -> &std::path::Path {
        &self.module_path
    }

    /// Drain the messages the cell has sent via `host_send`, oldest first.
    pub fn take_messages(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.env.as_mut(&mut self.store).outbox)
    }

    /// Execute the `_start` function (or any exported function name).
    /// Returns the result of the function as a string for debugging.
    ///
    /// # Errors
    /// Fails when the export is missing, is not `() -> i32`, or traps
    /// (including running out of [`CELL_FUEL`]).
    pub fn execute(&mut self, func_name: &str) -> Result<String> {
        let func = self
            .instance
            .exports
            .get_function(func_name)
            .with_context(|| format!("Function '{func_name}' not found in WASM module"))?;

        // Entry points take no params and return an i32 status.
        let typed = func
            .typed::<(), i32>(&self.store)
            .map_err(anyhow::Error::from)
            .with_context(|| "Failed to cast function signature")?;
        let ret = typed.call(&mut self.store)?;
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
    fn infinite_loops_run_out_of_fuel() {
        let wat = r#"(module
            (func (export "_start") (result i32)
                (loop $l (br $l))
                (i32.const 0)))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").is_err(), "must trap, not hang");
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
    fn a_tighter_declared_maximum_is_kept() {
        // Declared max 2 pages: growing to 3 must fail even though it is far
        // under the cap — the rewrite only ever lowers a maximum.
        let wat = r#"(module
            (memory (export "memory") 1 2)
            (func (export "_start") (result i32)
                (memory.grow (i32.const 2))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn initial_memory_above_the_cap_is_rejected() {
        // 5000 pages = ~312 MiB > MAX_CELL_MEMORY_BYTES.
        let wat = r#"(module (memory 5000) (func (export "_start") (result i32) (i32.const 0)))"#;
        assert!(WasmCell::from_bytes(wat.as_bytes()).is_err());
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
