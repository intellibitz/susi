//! Verify synthesized reflex intent correctness (VC-201-007).
//!
//! A source token is not evidence of behavior.  The probe compiles the
//! candidate to WASI in a private staging directory and executes the artifact
//! through the native sandbox.  Tests can inject a deterministic executor,
//! while production always uses the first-party sandbox client.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentFixture {
    pub input: String,
    pub expect_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflexVerdict {
    Accept,
    RejectMalformed,
    RejectCompilation,
    RejectExecution,
    RejectSignatureEcho,
    RejectWrongOutput,
}

/// Compile and execute a synthesized reflex through the real native sandbox.
pub fn probe_reflex(code: &str, fixture: &IntentFixture) -> ReflexVerdict {
    probe_reflex_with_executor(code, fixture, |wasm, input| {
        crate::susi_native::WasmHost::execute_reflex(wasm, input).map_err(|error| error.to_string())
    })
}

/// Compile and execute a reflex with an injected executor.  The compiler and
/// staging path remain real; the seam keeps unit tests independent of a
/// separately running `susi-native` process.
pub fn probe_reflex_with_executor(
    code: &str,
    fixture: &IntentFixture,
    execute: impl FnMut(&Path, &str) -> Result<String, String>,
) -> ReflexVerdict {
    if fixture.expect_action.trim().is_empty() {
        return ReflexVerdict::RejectMalformed;
    }
    let staging = match compile_to_wasi(code) {
        Ok(staging) => staging,
        Err(()) => return ReflexVerdict::RejectCompilation,
    };
    probe_compiled_with_executor(&staging.wasm, fixture, execute)
}

/// Execute an already compiled reflex through the native sandbox.  The
/// synthesis path uses this after its intent fixtures pass so publication has
/// both an independent output probe and the richer invariant checks.
pub fn probe_compiled_reflex(wasm: &Path, fixture: &IntentFixture) -> ReflexVerdict {
    probe_compiled_with_executor(wasm, fixture, |path, input| {
        crate::susi_native::WasmHost::execute_reflex(path, input).map_err(|error| error.to_string())
    })
}

fn probe_compiled_with_executor(
    wasm: &Path,
    fixture: &IntentFixture,
    mut execute: impl FnMut(&Path, &str) -> Result<String, String>,
) -> ReflexVerdict {
    let output = match execute(wasm, &fixture.input) {
        Ok(output) => output,
        Err(_) => return ReflexVerdict::RejectExecution,
    };
    let output = output.trim();
    if output == fixture.input.trim() {
        return ReflexVerdict::RejectSignatureEcho;
    }
    if output.contains(&fixture.expect_action) {
        ReflexVerdict::Accept
    } else {
        ReflexVerdict::RejectWrongOutput
    }
}

struct StagedReflex {
    root: PathBuf,
    wasm: PathBuf,
}

impl Drop for StagedReflex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn compile_to_wasi(code: &str) -> Result<StagedReflex, ()> {
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("susi-reflex-probe-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).map_err(|_| ())?;
    let source = root.join("reflex.rs");
    let wasm = root.join("reflex.wasm");
    if fs::write(&source, code).is_err() {
        let _ = fs::remove_dir_all(&root);
        return Err(());
    }
    let source = match source.to_str() {
        Some(path) => path,
        None => {
            let _ = fs::remove_dir_all(&root);
            return Err(());
        }
    };
    let wasm_path = match wasm.to_str() {
        Some(path) => path,
        None => {
            let _ = fs::remove_dir_all(&root);
            return Err(());
        }
    };
    let result = Command::new("rustc")
        .args(["--target", "wasm32-wasip1", "-O", "-o", wasm_path, source])
        .output();
    match result {
        Ok(output) if output.status.success() => Ok(StagedReflex { root, wasm }),
        _ => {
            let _ = fs::remove_dir_all(&root);
            Err(())
        }
    }
}
