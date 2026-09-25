//! WASM sandbox support for SUSI Swarm Cells.
//!
//! This module provides a minimal runtime for loading and executing
//! Rust‑compiled WebAssembly modules (cells) inside the SUSI daemon.
//! It uses the `wasmtime` crate with the Cranelift codegen backend.
//!
//! The design follows the Phase 1 goal of **WASM + namespace isolation**
//! while keeping the API simple for later extensions (e.g., capability
//! enforcement, resource limits).

use std::path::PathBuf;
use anyhow::{Context, Result};
use wasmtime::{Engine, Instance, Module, Store, Func, Caller};

/// Represents a loaded WASM cell.
///
/// * `module_path` – path to the compiled `.wasm` file.
/// * `engine` – shared Wasmtime engine (cached per‑process).
/// * `instance` – instantiated module ready for calls.
/// * `store` – the Wasmtime store holding the instance state.
pub struct WasmCell {
    #[allow(dead_code)] // retained for diagnostics and future hot-reload
    module_path: PathBuf,
    #[allow(dead_code)] // retained for engine-level config sharing and hot-reload
    engine: Engine,
    instance: Instance,
    store: Store<()>,
}

impl WasmCell {
    /// Load a WASM binary from `module_path` and instantiate it.
    ///
    /// The host exposes a `host_send` function that WASM modules can call
    /// to send messages back to the kernel (placeholder for full SUSI ABI).
    pub fn load(module_path: impl Into<PathBuf>) -> Result<Self> {
        let module_path = module_path.into();
        // Create a new engine (or reuse a global one in the future).
        let engine = Engine::default();
        let module = Module::from_file(&engine, &module_path)
            .with_context(|| format!("Failed to load WASM module {}", module_path.display()))?;

        // Define a simple host function that the WASM module can call to
        // send a message back to the kernel. In a full implementation this
        // would expose the full SUSI ABI.
        let mut store = Store::new(&engine, ());
        let host_send = Func::wrap(&mut store, move |_caller: Caller<'_, ()>, ptr: i32, len: i32| {
            // Placeholder: In a real cell we would read the memory at ptr/len
            // and forward the payload via the SUSI message bus.
            tracing::info!(
                "[WasmCell] host_send called – payload at {} (len {})",
                ptr,
                len,
            );
        });

        // Instantiate the module with the host function in imports.
        let instance = Instance::new(&mut store, &module, &[host_send.into()])
            .with_context(|| "Failed to instantiate WASM module")?;

        Ok(Self {
            module_path,
            engine,
            instance,
            store,
        })
    }

    /// Execute the `_start` function (or any exported function name).
    /// Returns the result of the function as a string for debugging.
    pub fn execute(&mut self, func_name: &str) -> Result<String> {
        let func = self
            .instance
            .get_func(&mut self.store, func_name)
            .with_context(|| format!("Function '{}' not found in WASM module", func_name))?;

        // For now we assume the function takes no params and returns i32.
        let typed = func
            .typed::<(), i32>(&self.store)
            .with_context(|| "Failed to cast function signature")?;
        let ret = typed.call(&mut self.store, ())?;
        Ok(format!("WASM function '{}' returned {}", func_name, ret))
    }
}

/// Spawn a WASM cell from a `.wasm` file at the given path.
///
/// The cell is loaded and instantiated but not yet executed — the caller
/// should invoke [`WasmCell::execute`] with the desired entry-point
/// (typically `"_start"`).
pub fn spawn_wasm_cell(wasm_path: PathBuf) -> Result<WasmCell> {
    WasmCell::load(wasm_path)
}
