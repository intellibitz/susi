//! Per-store limits and bounded input shared by cell and WASI execution.
use std::io::Read;
use std::path::Path;
use susi_error::{EaiError, EaiResult};
use wasmi::{StoreLimits, StoreLimitsBuilder};

pub(crate) const MAX_MODULE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_TABLE_ELEMENTS: usize = 1_000_000;

pub(crate) fn store_limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(crate::cell::MAX_CELL_MEMORY_BYTES)
        .memories(1)
        .instances(1)
        .tables(1)
        .table_elements(MAX_TABLE_ELEMENTS)
        .build()
}

/// The four-byte magic every binary WebAssembly module starts with (`\0asm`).
pub(crate) const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];

pub(crate) fn check_module_size(bytes: &[u8]) -> EaiResult<()> {
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(EaiError::sandbox(
            "WASM module exceeds the 64 MiB input limit",
        ));
    }
    Ok(())
}

/// The socket accepts binary modules only.
///
/// Until 2026-11-08 the host compiled the WebAssembly text format (`wat`) as
/// well, which put a text parser (`wat` + `wast`) in every shipped build for
/// an input no producer emits: reflex and cell producers compile `.wasm`
/// binaries, and the crate's own fixtures are compiled at test time
/// (`docs/lean-core.md`, "Compile budget"). Refusing here — before the memory
/// cap rewrite and before validation — names the contract instead of failing
/// deep inside the interpreter with a parser error.
pub(crate) fn check_binary_module(bytes: &[u8]) -> EaiResult<()> {
    if !bytes.starts_with(&WASM_MAGIC) {
        return Err(EaiError::sandbox(
            "not a binary WebAssembly module: the host accepts compiled `.wasm` input only",
        ));
    }
    Ok(())
}

pub(crate) fn read_module(path: &Path) -> EaiResult<Vec<u8>> {
    let file = std::fs::File::open(path)
        .map_err(|error| EaiError::sandbox(format!("opening WASM module: {error}")))?;
    let mut bytes = Vec::new();
    file.take(MAX_MODULE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| EaiError::sandbox(format!("reading WASM module: {error}")))?;
    check_module_size(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cell::WasmCell, wasm::WasmHost};
    use wasmi::{Engine, Linker, Module, Store};

    #[test]
    fn both_entrypoints_reject_multiple_memories_and_oversized_or_multiple_tables() {
        for (name, declarations) in [
            ("memories", "(memory 0) (memory 0)"),
            ("table_size", "(table 1000001 funcref)"),
            ("tables", "(table 0 funcref) (table 0 funcref)"),
        ] {
            let cell = format!(
                "(module {declarations} (func (export \"_start\") (result i32) i32.const 0))"
            );
            let binary = wat::parse_str(&cell).unwrap();
            assert!(
                Module::new(&Engine::default(), &binary).is_ok(),
                "invalid adversarial fixture {name}"
            );
            assert!(
                WasmCell::from_bytes(&binary).is_err(),
                "cell accepted {name}"
            );
            let wasi = format!("(module {declarations} (func (export \"_start\")))");
            let path = std::env::temp_dir()
                .join(format!("susi-limits-{name}-{}.wasm", std::process::id()));
            std::fs::write(&path, wat::parse_str(&wasi).unwrap()).unwrap();
            let result = WasmHost::execute_reflex(&path, "");
            std::fs::remove_file(path).unwrap();
            assert!(result.is_err(), "WASI accepted {name}");
        }
    }

    #[test]
    fn store_cannot_instantiate_a_second_module() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, store_limits());
        store.limiter(|limits| limits);
        let module = Module::new(&engine, wat::parse_str("(module)").unwrap()).unwrap();
        let linker = Linker::new(&engine);
        linker.instantiate_and_start(&mut store, &module).unwrap();
        assert!(linker.instantiate_and_start(&mut store, &module).is_err());
    }

    #[test]
    fn table_growth_is_bounded_on_both_entrypoints() {
        let cell = wat::parse_str(
            r#"(module (table 1 funcref)
            (func (export "_start") (result i32)
                (table.grow (ref.null func) (i32.const 1000000))))"#,
        )
        .unwrap();
        let mut instance = WasmCell::from_bytes(&cell).unwrap();
        assert_eq!(
            instance.execute("_start").unwrap(),
            "WASM function '_start' returned -1"
        );
        let wasi = wat::parse_str(
            r#"(module (table 1 funcref)
            (func (export "_start")
                (if (i32.ne (table.grow (ref.null func) (i32.const 1000000)) (i32.const -1))
                    (then unreachable))))"#,
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("susi-limits-growth-{}.wasm", std::process::id()));
        std::fs::write(&path, wasi).unwrap();
        let result = WasmHost::execute_reflex(&path, "");
        std::fs::remove_file(path).unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn oversized_file_is_rejected_before_parsing() {
        let path =
            std::env::temp_dir().join(format!("susi-limits-input-{}.wasm", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_MODULE_BYTES as u64 + 1)
            .unwrap();
        let error = read_module(&path).unwrap_err().to_string();
        std::fs::remove_file(path).unwrap();
        assert!(error.contains("64 MiB"));
    }

    #[test]
    fn max_module_bytes_constant_is_exact_64_mib() {
        assert_eq!(MAX_MODULE_BYTES, 64 * 1024 * 1024);
    }
}
