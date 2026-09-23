// SUSI Wasm Host Substrate
// Native Integration - High-performance reflex execution environment

use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;

pub struct WasmHost;

impl WasmHost {
    /// Pillar Sandbox: run untrusted Wasm (plugins or reflexes) under Wasmer/WASI.
    pub fn execute_untrusted_wasm(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        Self::execute_reflex(wasm_path, arg)
    }

    /// Executes a distilled reflex from a Wasm file via the standalone
    /// `susi-native` service (Wasmer/WASI isolation stays in that process).
    ///
    /// No local fallback: `wasmer`/`wasmer-wasix` are linked only into the
    /// service binary so feature planes never pull in the Wasm runtime.
    pub fn execute_reflex(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        match super::service::execute(wasm_path, arg) {
            Some(Ok(output)) => Ok(output),
            Some(Err(msg)) => Err(EaiError::process(format!("Wasm execution failed: {msg}"))),
            None => Err(EaiError::process(
                "susi-native service unreachable on 127.0.0.1:18084 \
                 (SUSI_NATIVE_PORT); start the service to run Wasm reflexes",
            )),
        }
    }
}
