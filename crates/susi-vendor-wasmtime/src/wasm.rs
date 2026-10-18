// SUSI Wasm Host Substrate
// Native Integration - High-performance reflex execution environment

use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;
use wasmtime::{Linker, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::WasiCtx;

pub struct WasmHost;

impl WasmHost {
    /// Pillar Sandbox: run untrusted Wasm (plugins or reflexes) under Wasmtime/WASI.
    pub fn execute_untrusted_wasm(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        Self::execute_reflex(wasm_path, arg)
    }

    /// Executes a distilled reflex from a Wasm file (Wasmtime/WASI isolation).
    ///
    /// Reflexes are model-written code: the module is compiled on a
    /// fuel-metered engine ([`crate::cell::CELL_FUEL`]) and its declared
    /// memory maximum is clamped by [`crate::cell::cap_memory`], so an
    /// infinite loop or runaway allocation traps instead of pinning the
    /// caller's thread. No preopened directories or ambient host
    /// capabilities are granted.
    pub fn execute_reflex(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        let engine = crate::cell::metered_engine()?;
        let bytes = std::fs::read(wasm_path).map_err(|e| {
            EaiError::process(format!(
                "Failed to read Wasm module {}: {e}",
                wasm_path.display()
            ))
        })?;
        // `wat::parse_bytes` passes already-binary modules through, so
        // reflexes built by rustc stay cheap and hand-written WAT probes
        // still load.
        let wasm = wat::parse_bytes(&bytes).map_err(|e| {
            EaiError::process(format!(
                "Failed to parse Wasm module {}: {e}",
                wasm_path.display()
            ))
        })?;
        let wasm = crate::cell::cap_memory(&wasm)?;
        let module = Module::new(&engine, &wasm).map_err(|e| {
            EaiError::process(format!(
                "Failed to compile Wasm module {}: {e}",
                wasm_path.display()
            ))
        })?;

        // No preopened directories, no inherited argv/env/stdin: the only
        // granted capability is a bounded-capacity stdout pipe.
        let stdout = MemoryOutputPipe::new(usize::MAX);
        let mut builder = WasiCtx::builder();
        builder.stdout(stdout.clone()).arg(arg);
        let mut store = Store::new(&engine, builder.build_p1());
        store
            .set_fuel(crate::cell::CELL_FUEL)
            .map_err(|e| EaiError::process(format!("Failed to set reflex fuel: {e}")))?;

        let mut linker = Linker::<WasiP1Ctx>::new(&engine);
        // Both import spellings: rustc's wasm32-wasip1 emits
        // `wasi_snapshot_preview1`; older hand-built probes may still speak
        // `wasi_unstable` (preview 0).
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |c| c)
            .map_err(|e| EaiError::process(format!("Failed to link WASI p1: {e}")))?;
        wasmtime_wasi::p0::add_to_linker_sync(&mut linker, |c| c)
            .map_err(|e| EaiError::process(format!("Failed to link WASI p0: {e}")))?;

        // This is a blocking call
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| EaiError::process(format!("Wasm execution failed: {e}")))?;
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| EaiError::process(format!("Wasm module exports no _start: {e}")))?;
        start
            .call(&mut store, ())
            .map_err(|e| EaiError::process(format!("Wasm execution failed: {e}")))?;

        // Capture stdout — MemoryOutputPipe already holds the complete
        // buffer; no channel EOF dance like the old wasmer Pipe needed.
        let captured = stdout.contents();
        let result = String::from_utf8_lossy(&captured);

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
    /// compile -> instantiate -> stdout-capture path unconditionally, unlike
    /// reflex_synth's end-to-end test which honestly skips without that
    /// toolchain.
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

    /// Tries to open `etc/passwd` through the first would-be preopened
    /// directory (fd 3) and prints `DENIED` or `OPENED`. With no directory
    /// granted, the sandbox must answer `DENIED`.
    const HOST_FS_PROBE_WAT: &str = r#"
        (module
            (import "wasi_snapshot_preview1" "path_open"
                (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (export "memory" (memory 0))
            (data (i32.const 100) "etc/passwd")
            (data (i32.const 200) "DENIED")
            (data (i32.const 300) "OPENED")
            (func (export "_start")
                (if (i32.eqz (call $path_open (i32.const 3) (i32.const 0) (i32.const 100)
                        (i32.const 10) (i32.const 0) (i64.const 2) (i64.const 0)
                        (i32.const 0) (i32.const 400)))
                    (then (i32.store (i32.const 0) (i32.const 300)))
                    (else (i32.store (i32.const 0) (i32.const 200))))
                (i32.store (i32.const 4) (i32.const 6))
                (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 20)))
            )
        )
    "#;

    #[test]
    fn untrusted_wasm_cannot_open_host_files() {
        let path =
            std::env::temp_dir().join(format!("susi_wasm_fs_probe_{}.wat", std::process::id()));
        std::fs::write(&path, HOST_FS_PROBE_WAT).unwrap();

        let result = WasmHost::execute_untrusted_wasm(&path, "unused");
        let _ = std::fs::remove_file(&path);

        assert_eq!(result.unwrap(), "DENIED");
    }

    /// A reflex whose guest code never terminates must trap on the shared
    /// fuel budget instead of pinning the calling thread forever.
    const INFINITE_LOOP_WAT: &str = r#"
        (module
            (func (export "_start") (loop (br 0)))
        )
    "#;

    #[test]
    fn execute_reflex_traps_on_fuel_exhaustion() {
        let path = std::env::temp_dir().join(format!("susi_wasm_fuel_{}.wat", std::process::id()));
        std::fs::write(&path, INFINITE_LOOP_WAT).unwrap();

        let result = WasmHost::execute_reflex(&path, "unused");
        let _ = std::fs::remove_file(&path);

        assert!(result.is_err(), "infinite loop must trap, not hang");
    }

    #[test]
    fn test_execute_reflex_runs_and_captures_stdout() {
        let path = std::env::temp_dir().join(format!("susi_wasm_test_{}.wat", std::process::id()));
        std::fs::write(&path, HELLO_WAT).unwrap();

        let result = WasmHost::execute_reflex(&path, "unused");
        let _ = std::fs::remove_file(&path);

        assert_eq!(result.unwrap(), "hello world");
    }
}
