//! In-process WASM cell runtime: fuel-metered, memory-capped, with a single
//! host import (`host_send`) exposed under both the `env` and `susi`
//! namespaces.
//!
//! The wasm engine is wasmtime. Fuel is metered natively
//! (`Config::consume_fuel` + `Store::set_fuel`) rather than by a compiler
//! middleware: wasmtime charges fuel per wasm instruction, so `CELL_FUEL`
//! keeps the same budget semantics the wasmer `Metering` middleware provided.
//! The declared-maximum memory cap is applied by rewriting the module's
//! memory section ([`cap_memory`]) before compile; a module that never
//! declared a maximum still cannot grow past [`MAX_CELL_MEMORY_BYTES`], and
//! imported memories are refused — the host provides none, so a cell
//! importing memory fails to instantiate.

use std::path::PathBuf;
use std::sync::LazyLock;
use susi_error::{EaiError, EaiResult};
use wasmparser::{Parser, Payload};
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store};

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

/// Instruction budget (fuel, roughly one unit per wasm instruction) per cell
/// or WASI reflex: an infinite loop traps instead of pinning a daemon thread
/// forever.
pub const CELL_FUEL: u64 = 10_000_000_000;

/// Engine with fuel metering compiled in; every store starts with
/// [`CELL_FUEL`]. Shared by cells and the WASI reflex runner (`wasm.rs`),
/// which executes model-written code on the same budget.
pub(crate) fn metered_engine() -> EaiResult<Engine> {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    Engine::new(&config)
        .map_err(|e| EaiError::sandbox(format!("Failed to create wasm engine: {e}")))
}

/// Clamp every defined memory's maximum to [`MAX_CELL_MEMORY_BYTES`].
///
/// Wasmtime enforces a memory's declared maximum on `memory.grow`, so capping
/// the declaration caps the cell — without a custom `Tunables`, whose
/// required `unsafe fn`s this crate's `unsafe_code` policy reserves for FFI.
/// Sections other than the memory section are copied through byte-for-byte.
/// Imported memories need no clamp: the host provides none, so such a cell
/// fails to instantiate. Shared with the WASI reflex runner (`wasm.rs`),
/// whose model-written modules get the same cap.
pub(crate) fn cap_memory(wasm: &[u8]) -> EaiResult<Vec<u8>> {
    let mut out = wasm_encoder::Module::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload =
            payload.map_err(|e| EaiError::sandbox(format!("Failed to parse WASM module: {e}")))?;
        if let Payload::Version {
            encoding: wasmparser::Encoding::Component,
            ..
        } = payload
        {
            return Err(EaiError::sandbox(
                "WASM components are not supported as cells",
            ));
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
                return Err(EaiError::sandbox(
                    "WASM section range does not fit in memory",
                ));
            };
            let data = wasm
                .get(start..end)
                .ok_or_else(|| EaiError::sandbox("WASM section range out of bounds"))?;
            out.section(&wasm_encoder::RawSection { id, data });
        }
    }
    Ok(out.finish())
}

/// Re-encode a memory section with each maximum clamped to the cell cap.
fn capped_memory_section(
    reader: wasmparser::MemorySectionReader<'_>,
) -> EaiResult<wasm_encoder::MemorySection> {
    let mut section = wasm_encoder::MemorySection::new();
    for memory in reader {
        let memory =
            memory.map_err(|e| EaiError::sandbox(format!("Failed to read memory section: {e}")))?;
        // Checked: this runs before wasmtime validates the module, so a
        // malformed page-size exponent must error, not overflow the shift.
        let page_bytes = 1u64
            .checked_shl(memory.page_size_log2.unwrap_or(16))
            .ok_or_else(|| EaiError::sandbox("invalid WASM memory page size"))?;
        let cap_pages = MAX_CELL_MEMORY_BYTES as u64 / page_bytes;
        if memory.initial > cap_pages {
            return Err(EaiError::sandbox(format!(
                "module memory starts at {} page(s), above the {} MiB cap",
                memory.initial,
                MAX_CELL_MEMORY_BYTES / (1024 * 1024)
            )));
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
/// * `store` – the wasmtime store holding the instance and the cell's
///   [`HostState`].
pub struct WasmCell {
    module_path: PathBuf,
    instance: Instance,
    store: Store<HostState>,
}

/// Copy a guest message out of the cell's exported memory into the outbox.
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
    let Some(memory) = caller.data().memory else {
        return SEND_NO_MEMORY;
    };
    let mut bytes = vec![0u8; len];
    // `read` bounds-checks `start + len` (overflow included) against the
    // current memory size.
    if memory.read(&caller, start, &mut bytes).is_err() {
        return SEND_OUT_OF_BOUNDS;
    }
    tracing::info!("[WasmCell] host_send received {} byte(s)", bytes.len());
    caller.data_mut().outbox.push(bytes);
    SEND_OK
}

impl WasmCell {
    /// Load a WASM binary from `module_path` and instantiate it.
    ///
    /// # Errors
    /// Fails when the file cannot be read or compiled, or instantiation fails.
    pub fn load(module_path: impl Into<PathBuf>) -> EaiResult<Self> {
        let module_path = module_path.into();
        let bytes = std::fs::read(&module_path).map_err(|e| {
            EaiError::sandbox(format!(
                "Failed to load WASM module {}: {e}",
                module_path.display()
            ))
        })?;
        Self::instantiate(&bytes, module_path)
    }

    /// Instantiate a cell from in-memory WASM (binary or text format).
    ///
    /// # Errors
    /// Fails when the bytes do not compile or instantiation fails.
    pub fn from_bytes(bytes: &[u8]) -> EaiResult<Self> {
        Self::instantiate(bytes, PathBuf::from("<memory>"))
    }

    fn instantiate(bytes: &[u8], module_path: PathBuf) -> EaiResult<Self> {
        // `wat::parse_bytes` passes already-binary modules through, so cells
        // built by rustc stay cheap and hand-written WAT still loads.
        let wasm = wat::parse_bytes(bytes)
            .map_err(|e| EaiError::sandbox(format!("Failed to parse WASM text: {e}")))?;
        let wasm = cap_memory(&wasm)?;
        let engine = metered_engine()?;
        let mut store = Store::new(&engine, HostState::default());
        store
            .set_fuel(CELL_FUEL)
            .map_err(|e| EaiError::sandbox(format!("Failed to set cell fuel: {e}")))?;
        let module = Module::new(&engine, &wasm).map_err(|e| {
            EaiError::sandbox(format!(
                "Failed to compile WASM module {}: {e}",
                module_path.display()
            ))
        })?;
        let mut linker = Linker::<HostState>::new(&engine);
        for namespace in ["env", "susi"] {
            linker
                .func_wrap(namespace, "host_send", host_send)
                .map_err(|e| EaiError::sandbox(format!("Failed to define host_send: {e}")))?;
        }
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| EaiError::sandbox(format!("Failed to instantiate WASM module: {e}")))?;
        // Bound after instantiation: a `start` function that calls
        // `host_send` sees no memory yet and gets SEND_NO_MEMORY.
        store.data_mut().memory = instance.get_memory(&mut store, "memory");
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
    /// Fails when the export is missing, is not `() -> i32`, or traps
    /// (including running out of [`CELL_FUEL`]).
    pub fn execute(&mut self, func_name: &str) -> EaiResult<String> {
        let func = self
            .instance
            .get_func(&mut self.store, func_name)
            .ok_or_else(|| {
                EaiError::sandbox(format!("Function '{func_name}' not found in WASM module"))
            })?;

        // Entry points take no params and return an i32 status.
        let typed = func
            .typed::<(), i32>(&self.store)
            .map_err(|e| EaiError::sandbox(format!("Failed to cast function signature: {e}")))?;
        let ret = typed
            .call(&mut self.store, ())
            .map_err(|e| EaiError::sandbox(format!("WASM function '{func_name}' trapped: {e}")))?;
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
pub fn spawn_wasm_cell(wasm_path: PathBuf) -> EaiResult<WasmCell> {
    WasmCell::load(wasm_path)
}

/// One default (Cranelift) engine for module validation: plugin hot-reload
/// compiles candidates to prove they load, and needs no metering.
static VALIDATION_ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);

/// Compile `wasm_bytes` (binary format) to prove it is a loadable module,
/// without instantiating it.
///
/// # Errors
/// The compile error text when the bytes are not a valid module.
pub fn validate_module(wasm_bytes: &[u8]) -> Result<(), String> {
    Module::validate(&VALIDATION_ENGINE, wasm_bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
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

    #[test]
    fn host_send_rejects_negative_arguments() {
        let wat = r#"(module
            (import "susi" "host_send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32)
                (call $send (i32.const -1) (i32.const 10))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn host_send_rejects_oversized_payload() {
        let wat = r#"(module
            (import "susi" "host_send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32)
                (call $send (i32.const 0) (i32.const 2000000))))"#;
        let mut cell = WasmCell::from_bytes(wat.as_bytes()).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn wasm_cell_load_file_lifecycle() {
        let missing = PathBuf::from("nonexistent_wasm_file_for_test.wasm");
        assert!(WasmCell::load(&missing).is_err());

        let wat = r#"(module (func (export "_start") (result i32) (i32.const 42)))"#;
        let tmp = std::env::temp_dir().join(format!("susi_cell_test_{}.wat", std::process::id()));
        std::fs::write(&tmp, wat).unwrap();
        let mut cell = WasmCell::load(&tmp).unwrap();
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned 42"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn host_send_rejects_when_outbox_full() {
        let mut cell = WasmCell::from_bytes(SENDER.as_bytes()).unwrap();
        for _ in 0..MAX_OUTBOX_MESSAGES {
            assert_eq!(
                cell.execute("_start").unwrap(),
                "WASM function '_start' returned 0"
            );
        }
        assert!(cell.execute("_start").unwrap().ends_with("returned -4"));
    }
}
