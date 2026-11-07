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
        Ok(output) if String::from_utf8_lossy(&output.stderr).contains("E0463") => {
            let fallback = Command::new("rustc")
                .args(["-O", "-o", wasm_path, source])
                .output();
            match fallback {
                Ok(out) if out.status.success() => Ok(StagedReflex { root, wasm }),
                _ => {
                    let _ = fs::remove_dir_all(&root);
                    Err(())
                }
            }
        }
        _ => {
            let _ = fs::remove_dir_all(&root);
            Err(())
        }
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn probe_reflex_with_executor_rejects_malformed_fixture_with_blank_expect_action() {
        let fixture = IntentFixture {
            input: "ping".into(),
            expect_action: "   ".into(),
        };
        let verdict =
            probe_reflex_with_executor("fn main() {}", &fixture, |_, _| Ok("pong".into()));
        assert_eq!(verdict, ReflexVerdict::RejectMalformed);
    }

    #[test]
    fn probe_compiled_with_executor_handles_execution_error_and_signature_echo() {
        let dummy = Path::new("reflex.wasm");
        let fixture = IntentFixture {
            input: "transform:alpha".into(),
            expect_action: "alpha_done".into(),
        };
        let exec_err =
            probe_compiled_with_executor(dummy, &fixture, |_, _| Err("sandbox trap".into()));
        assert_eq!(exec_err, ReflexVerdict::RejectExecution);

        let echo =
            probe_compiled_with_executor(dummy, &fixture, |_, input| Ok(format!("  {input}  ")));
        assert_eq!(echo, ReflexVerdict::RejectSignatureEcho);
    }

    #[test]
    fn probe_compiled_with_executor_distinguishes_accept_from_wrong_output() {
        let dummy = Path::new("reflex.wasm");
        let fixture = IntentFixture {
            input: "compute".into(),
            expect_action: "RESULT=42".into(),
        };
        let wrong = probe_compiled_with_executor(dummy, &fixture, |_, _| Ok("RESULT=0".into()));
        assert_eq!(wrong, ReflexVerdict::RejectWrongOutput);

        let accepted = probe_compiled_with_executor(dummy, &fixture, |_, _| {
            Ok("ok: RESULT=42 verified".into())
        });
        assert_eq!(accepted, ReflexVerdict::Accept);
    }

    #[test]
    fn probe_reflex_with_executor_rejects_invalid_rust_source_before_executor_runs() {
        let fixture = IntentFixture {
            input: "hello".into(),
            expect_action: "world".into(),
        };
        let mut called = false;
        let verdict = probe_reflex_with_executor("not valid rust source {{{", &fixture, |_, _| {
            called = true;
            Ok("world".into())
        });
        assert_eq!(verdict, ReflexVerdict::RejectCompilation);
        assert!(!called);
    }

    #[test]
    fn intent_fixture_round_trips_serde_json_and_empty_expect_action_is_malformed() {
        let fixture = IntentFixture {
            input: "status".into(),
            expect_action: "ok".into(),
        };
        let encoded = serde_json::to_string(&fixture).expect("serialize IntentFixture");
        let decoded: IntentFixture =
            serde_json::from_str(&encoded).expect("deserialize IntentFixture");
        assert_eq!(decoded, fixture);

        let empty_action = IntentFixture {
            input: "status".into(),
            expect_action: String::new(),
        };
        let mut invoked = false;
        let verdict = probe_reflex_with_executor("invalid rust", &empty_action, |_, _| {
            invoked = true;
            Ok("ok".into())
        });
        assert_eq!(verdict, ReflexVerdict::RejectMalformed);
        assert!(!invoked);
    }
}
