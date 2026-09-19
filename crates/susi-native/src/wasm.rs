// SUSI Wasm Host Substrate
// Native Integration - High-performance reflex execution environment

use std::path::Path;
use susi_error::{EaiError, EaiResult};
use wasmer::{Engine, Module};
use wasmer_types::ModuleHash;
use wasmer_wasix::runners::wasi::{RuntimeOrEngine, WasiRunner};
use wasmer_wasix::Pipe;

pub struct WasmHost;

impl WasmHost {
    /// Executes a distilled reflex from a Wasm file
    pub fn execute_reflex(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        let engine = Engine::default();
        let module = Module::from_file(&engine, wasm_path)
            .map_err(|e| EaiError::process(format!("Failed to load Wasm module: {}", e)))?;

        let (stdout_tx, stdout_rx) = Pipe::channel();

        let mut runner = WasiRunner::new();
        runner.with_stdout(Box::new(stdout_tx)).with_args([arg]);

        // This is a blocking call
        runner
            .run_wasm(
                RuntimeOrEngine::Engine(engine),
                "susi-reflex",
                module,
                ModuleHash::random(),
            )
            .map_err(|e| EaiError::process(format!("Wasm execution failed: {}", e)))?;
        // WasiRunner keeps its own Arc-cloned handle to the stdout pipe
        // (ArcBoxFile); without dropping it here the sender never closes and
        // the read below blocks forever waiting for EOF that never comes.
        drop(runner);

        // Capture stdout
        let mut result = String::new();
        use std::io::Read;
        let mut reader = stdout_rx;
        reader
            .read_to_string(&mut result)
            .map_err(|e| EaiError::process(format!("Failed to read Wasm output: {}", e)))?;

        Ok(if result.trim().is_empty() {
            "Wasm execution success (No Output)".to_string()
        } else {
            result.trim().to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-written WAT (not compiled via rustc's wasm32-wasip1 target, which
    /// isn't installed in this environment) so this exercises the real
    /// Module::from_file -> WasiRunner::run_wasm -> Pipe stdout-capture path
    /// unconditionally, unlike reflex_synth's end-to-end test which honestly
    /// skips without that toolchain.
    const HELLO_WAT: &str = r#"
        (module
            (import "wasi_unstable" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (export "memory" (memory 0))
            (data (i32.const 8) "hello world")
            (func $main (export "_start")
                (i32.store (i32.const 0) (i32.const 8))
                (i32.store (i32.const 4) (i32.const 11))
                (call $fd_write
                    (i32.const 1)
                    (i32.const 0)
                    (i32.const 1)
                    (i32.const 20)
                )
                drop
            )
        )
    "#;

    #[test]
    fn test_execute_reflex_runs_and_captures_stdout() {
        let path = std::env::temp_dir().join(format!("susi_wasm_test_{}.wat", std::process::id()));
        std::fs::write(&path, HELLO_WAT).unwrap();

        let result = WasmHost::execute_reflex(&path, "unused");
        let _ = std::fs::remove_file(&path);

        assert_eq!(result.unwrap(), "hello world");
    }
}
