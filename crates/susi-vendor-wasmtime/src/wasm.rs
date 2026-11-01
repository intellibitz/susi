//! Native Rust interpreter host for capability-limited WASI reflexes.
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;
use wasmi::{Linker, Module, Store};

pub struct WasmHost;

impl WasmHost {
    /// Execute untrusted WASI without inherited host capabilities.
    pub fn execute_untrusted_wasm(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        Self::execute_reflex(wasm_path, arg)
    }

    /// Run a fuel-metered, memory-capped Rust/WASI reflex. Stdout is capped
    /// at 16 MiB and no directories, sockets, environment or stdin are inherited.
    pub fn execute_reflex(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        let engine = crate::cell::metered_engine()?;
        let bytes = std::fs::read(wasm_path)
            .map_err(|error| EaiError::process(format!("reading Wasm: {error}")))?;
        let wasm = wat::parse_bytes(&bytes)
            .map_err(|error| EaiError::process(format!("parsing Wasm: {error}")))?;
        let wasm = crate::cell::cap_memory(&wasm)?;
        let module = Module::new(&engine, &wasm)
            .map_err(|error| EaiError::process(format!("validating Wasm: {error}")))?;
        let context = crate::wasi::Context::new(arg).map_err(EaiError::process)?;
        let mut store = Store::new(&engine, context);
        store
            .set_fuel(crate::cell::CELL_FUEL)
            .map_err(|error| EaiError::process(format!("setting fuel: {error}")))?;
        let mut linker = Linker::new(&engine);
        crate::wasi::link(&mut linker, &module)
            .map_err(|error| EaiError::process(format!("linking WASI: {error}")))?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|error| EaiError::process(format!("instantiating Wasm: {error}")))?;
        store.data_mut().memory = instance.get_memory(&store, "memory");
        let start = instance
            .get_typed_func::<(), ()>(&store, "_start")
            .map_err(|error| EaiError::process(format!("missing _start: {error}")))?;
        if let Err(error) = start.call(&mut store, ()) {
            if error.i32_exit_status() != Some(0) {
                return Err(EaiError::process(format!("Wasm execution failed: {error}")));
            }
        }
        let result = String::from_utf8_lossy(&store.data().stdout);
        Ok(if result.trim().is_empty() {
            "Wasm execution success (No Output)".to_owned()
        } else {
            result.trim().to_owned()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "explicit qualification requires the wasm32-wasip1 Rust target"]
    fn compiled_rust_wasi_fixture_keeps_arguments_and_denies_host_state() {
        let dir = std::env::temp_dir().join(format!("susi-wasmi-rust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("reflex.rs");
        let wasm = dir.join("reflex.wasm");
        std::fs::write(
            &source,
            r#"
            fn main() {
                assert_eq!(std::env::vars_os().count(), 0);
                assert!(std::fs::read("Cargo.toml").is_err());
                let start = std::time::Instant::now();
                std::thread::sleep(std::time::Duration::from_millis(1));
                assert!(start.elapsed().as_secs() < 60);
                println!("native:{}", std::env::args().nth(1).unwrap());
                std::process::exit(0);
            }
        "#,
        )
        .unwrap();
        let output = std::process::Command::new("rustc")
            .args(["--edition", "2021", "--target", "wasm32-wasip1", "-O"])
            .arg(&source)
            .arg("-o")
            .arg(&wasm)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            WasmHost::execute_reflex(&wasm, "isolated").unwrap(),
            "native:isolated"
        );
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(wasm).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn unsupported_wasi_never_reports_success_and_exit_zero_is_success() {
        let path =
            std::env::temp_dir().join(format!("susi-wasi-unsupported-{}.wat", std::process::id()));
        std::fs::write(&path, r#"(module
            (import "wasi_snapshot_preview1" "unsupported_operation" (func $poll (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (memory (export "memory") 1)
            (func (export "_start")
                (if (i32.ne (call $poll (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)) (i32.const 52)) (then unreachable))
                (call $exit (i32.const 0))))"#).unwrap();
        assert_eq!(
            WasmHost::execute_reflex(&path, "").unwrap(),
            "Wasm execution success (No Output)"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_guest_pointer_traps_no_host_and_returns_fault() {
        let path = std::env::temp_dir().join(format!("susi-wasi-fault-{}.wat", std::process::id()));
        std::fs::write(&path, r#"(module
            (import "wasi_snapshot_preview1" "args_sizes_get" (func $args (param i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "_start")
                (if (i32.ne (call $args (i32.const -1) (i32.const 4)) (i32.const 21)) (then unreachable))))"#).unwrap();
        assert!(WasmHost::execute_reflex(&path, "").is_ok());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn output_and_host_waits_are_bounded() {
        let path =
            std::env::temp_dir().join(format!("susi-wasi-bounds-{}.wat", std::process::id()));
        std::fs::write(&path, r#"(module
            (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 257)
            (func (export "_start")
                (i32.store (i32.const 0) (i32.const 4096))
                (i32.store (i32.const 4) (i32.const 16777217))
                (if (i32.ne (call $write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 64)) (i32.const 76)) (then unreachable))
                (i32.store (i32.const 0) (i32.const 0))
                (i32.store (i32.const 4) (i32.const 0))
                (i32.store (i32.const 16) (i32.const 1))
                (i64.store (i32.const 24) (i64.const 60000000000))
                (if (i32.ne (call $poll (i32.const 0) (i32.const 128) (i32.const 1) (i32.const 256)) (i32.const 76)) (then unreachable))))"#).unwrap();
        assert_eq!(
            WasmHost::execute_reflex(&path, "").unwrap(),
            "Wasm execution success (No Output)"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_wasi_signature_is_rejected_before_execution() {
        let path =
            std::env::temp_dir().join(format!("susi-wasi-signature-{}.wat", std::process::id()));
        std::fs::write(
            &path,
            r#"(module
            (import "wasi_snapshot_preview1" "fd_write" (func (param i64) (result i32)))
            (func (export "_start")))"#,
        )
        .unwrap();
        assert!(WasmHost::execute_reflex(&path, "")
            .unwrap_err()
            .to_string()
            .contains("invalid WASI signature"));
        std::fs::remove_file(path).unwrap();
    }

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

    /// Prints `argv[1]`, the slot a reflex reads its input from. The caller's
    /// argument must land there and not in the program slot: putting it first
    /// made every generated reflex report an empty input.
    const ARGV_WAT: &str = r#"
        (module
            (import "wasi_snapshot_preview1" "args_sizes_get"
                (func $args_sizes_get (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "args_get"
                (func $args_get (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory 1)
            (export "memory" (memory 0))
            (func (export "_start")
                (drop (call $args_sizes_get (i32.const 0) (i32.const 4)))
                (drop (call $args_get (i32.const 64) (i32.const 4096)))
                ;; iovec = { ptr = argv[1], len = 12 }
                (i32.store (i32.const 300) (i32.load (i32.const 68)))
                (i32.store (i32.const 304) (i32.const 12))
                (drop (call $fd_write (i32.const 1) (i32.const 300)
                        (i32.const 1) (i32.const 320)))
            )
        )
    "#;

    #[test]
    fn a_reflex_argument_is_argv_one_not_the_program_name() {
        let path = std::env::temp_dir().join(format!("susi_wasm_argv_{}.wat", std::process::id()));
        std::fs::write(&path, ARGV_WAT).unwrap();
        let result = WasmHost::execute_reflex(&path, "the-argument");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            result.unwrap(),
            "the-argument",
            "argv must be [program, argument]: argv[1] is what a reflex reads"
        );
    }

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
