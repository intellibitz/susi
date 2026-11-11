//! In-process WASM cell runtime: fuel-metered, memory-capped, with a single
//! host import (`host_send`) exposed under both the `env` and `susi`
//! namespaces.
//!
//! The wasm engine is Wasmi. Fuel is metered natively
//! (`Config::consume_fuel` + `Store::set_fuel`) rather than by a compiler
//! middleware. Interpreter fuel units differ from the previous JIT; the
//! bounded budget is qualified by real Rust/WASI and infinite-loop tests.
//! The declared-maximum memory cap is applied by rewriting the module's
//! memory section ([`cap_memory`]) before compile; a module that never
//! declared a maximum still cannot grow past [`MAX_CELL_MEMORY_BYTES`], and
//! imported memories are refused — the host provides none, so a cell
//! importing memory fails to instantiate.

use std::path::PathBuf;
use std::sync::LazyLock;
use susi_error::{EaiError, EaiResult};
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store};

/// Largest single `host_send` payload the host accepts.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Most undrained messages a cell may queue before sends are rejected.
pub const MAX_OUTBOX_MESSAGES: usize = 1024;
/// Maximum aggregate payload bytes retained in the undrained host outbox.
pub const MAX_OUTBOX_BYTES: usize = crate::resources::MAX_OUTPUT_BYTES;

/// `host_send` return codes.
const SEND_OK: i32 = 0;
const SEND_BAD_ARGS: i32 = -1;
const SEND_NO_MEMORY: i32 = -2;
const SEND_OUT_OF_BOUNDS: i32 = -3;
const SEND_OUTBOX_FULL: i32 = -4;

/// Per-cell host state reachable from `host_send`.
struct HostState {
    limits: wasmi::StoreLimits,
    outbox: Vec<Vec<u8>>,
    outbox_bytes: usize,
    /// The cell's exported `memory`, bound right after instantiation.
    memory: Option<Memory>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            outbox: Vec::new(),
            outbox_bytes: 0,
            memory: None,
            limits: crate::resources::store_limits(),
        }
    }
}

/// Linear-memory cap per cell: a runaway cell traps instead of growing the
/// daemon's own memory without bound.
pub const MAX_CELL_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Instruction budget (fuel, roughly one unit per wasm instruction) per cell
/// or WASI reflex: an infinite loop traps instead of pinning a daemon thread
/// forever.
pub const CELL_FUEL: u64 = 10_000_000;

/// Engine with fuel metering compiled in; every store starts with
/// [`CELL_FUEL`]. Shared by cells and the WASI reflex runner (`wasm.rs`),
/// which executes model-written code on the same budget.
pub(crate) fn metered_engine() -> EaiResult<Engine> {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    Ok(Engine::new(&config))
}

/// Clamp every defined memory's maximum to [`MAX_CELL_MEMORY_BYTES`].
///
/// Wasmi enforces declared maxima on `memory.grow`. StoreLimits additionally
/// permits only one memory and bounds tables and instance count, so adding
/// memories cannot multiply the per-cell budget.
/// Sections other than the memory section are copied through byte-for-byte.
/// Imported memories need no clamp: the host provides none, so such a cell
/// fails to instantiate. Shared with the WASI reflex runner (`wasm.rs`),
/// whose model-written modules get the same cap.
pub(crate) fn cap_memory(wasm: &[u8]) -> EaiResult<Vec<u8>> {
    crate::resources::check_module_size(wasm)?;
    let header = wasm
        .get(..8)
        .ok_or_else(|| parse_error("unexpected end of the module header"))?;
    if header[..4] != crate::resources::WASM_MAGIC {
        return Err(parse_error("magic header not detected"));
    }
    match (
        u16::from_le_bytes([header[4], header[5]]),
        u16::from_le_bytes([header[6], header[7]]),
    ) {
        (WASM_MODULE_VERSION, KIND_MODULE) => {}
        (_, KIND_COMPONENT) => {
            return Err(EaiError::sandbox(
                "WASM components are not supported as cells",
            ))
        }
        (version, _) => {
            return Err(parse_error(&format!(
                "unknown binary version: {version:#x}"
            )))
        }
    }
    let mut out = Vec::with_capacity(wasm.len());
    out.extend_from_slice(&crate::resources::WASM_MAGIC);
    out.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = usize::try_from(read_leb(wasm, &mut at, 32)?)
            .map_err(|_| parse_error("section size does not fit in memory"))?;
        let data = at
            .checked_add(size)
            .and_then(|end| wasm.get(at..end))
            .ok_or_else(|| parse_error("section too large"))?;
        at += size;
        let rewritten;
        let body = if id == MEMORY_SECTION {
            rewritten = capped_memory_section(data)?;
            rewritten.as_slice()
        } else {
            data
        };
        out.push(id);
        write_leb(&mut out, body.len() as u64);
        out.extend_from_slice(body);
    }
    Ok(out)
}

/// Binary-format constants (WebAssembly core spec, section 5.5; the
/// component-model layer byte is 1).
const WASM_MODULE_VERSION: u16 = 0x1;
const KIND_MODULE: u16 = 0x0;
const KIND_COMPONENT: u16 = 0x1;
const MEMORY_SECTION: u8 = 5;

fn parse_error(detail: &str) -> EaiError {
    EaiError::sandbox(format!("Failed to parse WASM module: {detail}"))
}

/// Unsigned LEB128 of at most `bits` bits, advancing `at`.
fn read_leb(bytes: &[u8], at: &mut usize, bits: u32) -> EaiResult<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes
            .get(*at)
            .ok_or_else(|| parse_error("unexpected end of a LEB128 integer"))?;
        *at += 1;
        let low = u64::from(byte & 0x7f);
        if shift >= bits || (bits - shift < 7 && low >> (bits - shift) != 0) {
            return Err(parse_error("integer representation too long or too large"));
        }
        value |= low << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

fn write_leb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Re-encode a memory section with each maximum clamped to the cell cap.
///
/// Each entry is `flags` (bit 0 maximum, 1 shared, 2 memory64, 3 custom page
/// size), the initial size, the optional maximum and the optional page-size
/// exponent; sizes are 64-bit LEB128 for memory64 and 32-bit otherwise.
fn capped_memory_section(data: &[u8]) -> EaiResult<Vec<u8>> {
    let mut at = 0;
    let count = read_leb(data, &mut at, 32)?;
    let mut out = Vec::with_capacity(data.len() + 8);
    write_leb(&mut out, count);
    for _ in 0..count {
        let flags = *data
            .get(at)
            .ok_or_else(|| parse_error("unexpected end of the memory section"))?;
        at += 1;
        if flags & !0b1111 != 0 {
            return Err(parse_error("invalid memory limits flags"));
        }
        let bits = if flags & 0b0100 != 0 { 64 } else { 32 };
        let initial = read_leb(data, &mut at, bits)?;
        let maximum = if flags & 0b0001 != 0 {
            Some(read_leb(data, &mut at, bits)?)
        } else {
            None
        };
        let page_size_log2 = if flags & 0b1000 != 0 {
            let log2 = read_leb(data, &mut at, 32)?;
            if log2 >= 64 {
                return Err(parse_error("invalid custom page size"));
            }
            Some(log2)
        } else {
            None
        };
        // The exponent is below 64 (checked above, before Wasmi validates
        // the module), so the shift cannot overflow.
        let page_bytes = 1u64 << page_size_log2.unwrap_or(16);
        let cap_pages = MAX_CELL_MEMORY_BYTES as u64 / page_bytes;
        if initial > cap_pages {
            return Err(EaiError::sandbox(format!(
                "module memory starts at {initial} page(s), above the {} MiB cap",
                MAX_CELL_MEMORY_BYTES / (1024 * 1024)
            )));
        }
        out.push(flags | 0b0001);
        write_leb(&mut out, initial);
        write_leb(&mut out, maximum.map_or(cap_pages, |m| m.min(cap_pages)));
        if let Some(log2) = page_size_log2 {
            write_leb(&mut out, log2);
        }
    }
    if at != data.len() {
        return Err(parse_error("unexpected content in the memory section"));
    }
    Ok(out)
}

/// Represents a loaded WASM cell.
///
/// * `module_path` – path to the compiled `.wasm` file.
/// * `instance` – instantiated module ready for calls.
/// * `store` – the Wasmi store holding the instance and the cell's
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
    if caller.data().outbox.len() >= MAX_OUTBOX_MESSAGES
        || len > MAX_OUTBOX_BYTES.saturating_sub(caller.data().outbox_bytes)
    {
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
    let state = caller.data_mut();
    state.outbox_bytes += len;
    state.outbox.push(bytes);
    SEND_OK
}

impl WasmCell {
    /// Load a WASM binary from `module_path` and instantiate it.
    ///
    /// # Errors
    /// Fails when the file cannot be read or compiled, or instantiation fails.
    pub fn load(module_path: impl Into<PathBuf>) -> EaiResult<Self> {
        let module_path = module_path.into();
        let bytes = crate::resources::read_module(&module_path)?;
        Self::instantiate(&bytes, module_path)
    }

    /// Instantiate a cell from an in-memory binary WASM module.
    ///
    /// # Errors
    /// Fails when the bytes are not a binary module, do not compile, or
    /// instantiation fails.
    pub fn from_bytes(bytes: &[u8]) -> EaiResult<Self> {
        Self::instantiate(bytes, PathBuf::from("<memory>"))
    }

    fn instantiate(bytes: &[u8], module_path: PathBuf) -> EaiResult<Self> {
        crate::resources::check_module_size(bytes)?;
        crate::resources::check_binary_module(bytes)?;
        let wasm = cap_memory(bytes)?;
        let engine = metered_engine()?;
        let mut store = Store::new(&engine, HostState::default());
        store.limiter(|state| &mut state.limits);
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
            .instantiate_and_start(&mut store, &module)
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
        let state = self.store.data_mut();
        state.outbox_bytes = 0;
        std::mem::take(&mut state.outbox)
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

/// One default Wasmi engine for module validation: plugin hot-reload
/// compiles candidates to prove they load, and needs no metering.
static VALIDATION_ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);

/// Validate `wasm_bytes` structure and input size without instantiating it.
/// Store resource limits and import capabilities are checked at instantiation.
///
/// # Errors
/// The compile error text when the bytes are not a valid module.
pub fn validate_module(wasm_bytes: &[u8]) -> Result<(), String> {
    crate::resources::check_module_size(wasm_bytes).map_err(|error| error.to_string())?;
    Module::validate(&VALIDATION_ENGINE, wasm_bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile a readable test fixture to the bytes the host accepts. Test
    /// only: the product boundary takes compiled modules, so every fixture
    /// crosses `wat` here and nothing else.
    fn module(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("test fixture compiles")
    }

    #[test]
    fn queued_payload_budget_refuses_growth_and_drain_restores_capacity() {
        let wat = r#"(module
            (import "env" "host_send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 16)
            (func (export "_start") (result i32) (call $send (i32.const 0) (i32.const 1048576))))"#;
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        for _ in 0..MAX_OUTBOX_BYTES / MAX_MESSAGE_BYTES {
            assert_eq!(
                cell.execute("_start").unwrap(),
                "WASM function '_start' returned 0"
            );
        }
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned -4"
        );
        let messages = cell.take_messages();
        assert_eq!(
            messages.iter().map(Vec::len).sum::<usize>(),
            MAX_OUTBOX_BYTES
        );
        drop(messages);
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned 0"
        );
        assert_eq!(cell.take_messages().len(), 1);
    }

    const SENDER: &str = r#"(module
        (import "env" "host_send" (func $send (param i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 16) "hello host")
        (func (export "_start") (result i32)
            (call $send (i32.const 16) (i32.const 10))))"#;

    #[test]
    fn host_send_copies_guest_payload_into_outbox() {
        let mut cell = WasmCell::from_bytes(&module(SENDER)).unwrap();
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
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -3"));
        assert!(cell.take_messages().is_empty());
    }

    #[test]
    fn host_send_without_exported_memory_is_rejected() {
        let wat = r#"(module
            (import "env" "host_send" (func $send (param i32 i32) (result i32)))
            (func (export "_start") (result i32)
                (call $send (i32.const 0) (i32.const 1))))"#;
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -2"));
    }

    #[test]
    fn infinite_loops_run_out_of_fuel() {
        let wat = r#"(module
            (func (export "_start") (result i32)
                (loop $l (br $l))
                (i32.const 0)))"#;
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
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
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
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
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn initial_memory_above_the_cap_is_rejected() {
        // 5000 pages = ~312 MiB > MAX_CELL_MEMORY_BYTES.
        let wat = r#"(module (memory 5000) (func (export "_start") (result i32) (i32.const 0)))"#;
        assert!(WasmCell::from_bytes(&module(wat)).is_err());
    }

    #[test]
    fn cells_without_imports_instantiate() {
        let wat = r#"(module (func (export "_start") (result i32) (i32.const 7)))"#;
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
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
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn host_send_rejects_oversized_payload() {
        let wat = r#"(module
            (import "susi" "host_send" (func $send (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start") (result i32)
                (call $send (i32.const 0) (i32.const 2000000))))"#;
        let mut cell = WasmCell::from_bytes(&module(wat)).unwrap();
        assert!(cell.execute("_start").unwrap().ends_with("returned -1"));
    }

    #[test]
    fn wasm_cell_load_file_lifecycle() {
        let missing = PathBuf::from("nonexistent_wasm_file_for_test.wasm");
        assert!(WasmCell::load(&missing).is_err());

        let wat = r#"(module (func (export "_start") (result i32) (i32.const 42)))"#;
        let tmp = std::env::temp_dir().join(format!("susi_cell_test_{}.wasm", std::process::id()));
        std::fs::write(&tmp, module(wat)).unwrap();
        let mut cell = WasmCell::load(&tmp).unwrap();
        assert_eq!(
            cell.execute("_start").unwrap(),
            "WASM function '_start' returned 42"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn a_text_format_module_is_refused_by_name() {
        // The host accepts compiled modules only; a `.wat` file that reaches
        // it fails before the interpreter with the contract in the message.
        let error = match WasmCell::from_bytes(br#"(module (func (export "_start")))"#) {
            Ok(_) => panic!("a text-format module must not load"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("binary WebAssembly module"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn host_send_rejects_when_outbox_full() {
        let mut cell = WasmCell::from_bytes(&module(SENDER)).unwrap();
        for _ in 0..MAX_OUTBOX_MESSAGES {
            assert_eq!(
                cell.execute("_start").unwrap(),
                "WASM function '_start' returned 0"
            );
        }
        assert!(cell.execute("_start").unwrap().ends_with("returned -4"));
    }

    /// The rewrite clamps an unbounded memory, keeps every other section
    /// byte-for-byte and still yields a module Wasmi validates.
    #[test]
    fn cap_memory_adds_the_cap_and_copies_other_sections() {
        let wasm = module(SENDER);
        let capped = cap_memory(&wasm).unwrap();
        validate_module(&capped).unwrap();
        // Memory section of the fixture: one memory, flags 0, initial 1.
        let original_memory = [0x05, 0x03, 0x01, 0x00, 0x01];
        let cap_pages = (MAX_CELL_MEMORY_BYTES / 65536) as u64;
        let mut expected_memory = vec![0x05];
        let mut body = vec![0x01, 0x01, 0x01];
        write_leb(&mut body, cap_pages);
        write_leb(&mut expected_memory, body.len() as u64);
        expected_memory.extend_from_slice(&body);
        let at = wasm
            .windows(original_memory.len())
            .position(|w| w == original_memory)
            .unwrap();
        let mut expected = wasm[..at].to_vec();
        expected.extend_from_slice(&expected_memory);
        expected.extend_from_slice(&wasm[at + original_memory.len()..]);
        assert_eq!(capped, expected);
        // Idempotent: a capped module is already at the cap.
        assert_eq!(cap_memory(&capped).unwrap(), capped);
    }

    #[test]
    fn cap_memory_refuses_components_and_malformed_input() {
        let component = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];
        let error = cap_memory(&component).unwrap_err().to_string();
        assert!(error.contains("components are not supported"), "{error}");
        for bad in [
            &[0x00, 0x61, 0x73, 0x6d, 0x02, 0x00, 0x00, 0x00][..],
            &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00][..],
            // Section claims more bytes than remain.
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x00,
            ][..],
            // Memory section with an unknown flag bit.
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x10, 0x01,
            ][..],
            // Memory section with trailing bytes after its one entry.
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x00, 0x01, 0x00,
            ][..],
            // Custom page size exponent 64.
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x08, 0x01, 0x40,
            ][..],
            // Over-long 32-bit LEB128 initial size.
            &[
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x08, 0x01, 0x00, 0xff, 0xff,
                0xff, 0xff, 0xff, 0x01,
            ][..],
        ] {
            let error = cap_memory(bad).unwrap_err().to_string();
            assert!(error.contains("Failed to parse WASM module"), "{error}");
        }
    }

    #[test]
    fn cap_memory_honours_custom_page_sizes_and_memory64_widths() {
        // One memory, custom 1-byte pages (log2 0), initial 1, no maximum:
        // the cap is MAX_CELL_MEMORY_BYTES pages of one byte.
        let wasm = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x08, 0x01, 0x00,
        ];
        let capped = cap_memory(&wasm).unwrap();
        let mut body = vec![0x01, 0x09, 0x01];
        write_leb(&mut body, MAX_CELL_MEMORY_BYTES as u64);
        body.push(0x00);
        assert_eq!(&capped[8..10], &[0x05, body.len() as u8]);
        assert_eq!(&capped[10..], body.as_slice());
        // memory64 sizes may exceed 32 bits; above the cap they are refused
        // by size, not as a parse error.
        let mut wasm64 = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05];
        let mut section = vec![0x01, 0x04];
        write_leb(&mut section, 1u64 << 40);
        write_leb(&mut wasm64, section.len() as u64);
        wasm64.extend_from_slice(&section);
        let error = cap_memory(&wasm64).unwrap_err().to_string();
        assert!(error.contains("above the 256 MiB cap"), "{error}");
    }
}
