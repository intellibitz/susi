// SUSI Reflex Synthesizer
// Test-Driven Evolution Substrate, implementing identity.json Mandate 20's
// Alpha-Self Evolution Order (Motion -> Architecture -> Structure -> Logic) —
// not the unrelated release-gate "Motion Rule" (Pillar IV item 3); this file
// predates the renaming that split those two concepts apart and originally
// called this one "Motion Rule Protocol" too, exactly the collision
// Mandate 20's own note warns about.

use crate::susi_error::{EaiError, EaiResult};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct ReflexSynthesizer;

/// Basename-only slug for reflex files — rejects `..`, separators, and
/// non-identifier characters that could escape the reflexes directory.
fn sanitize_reflex_slug(intent: &str) -> EaiResult<String> {
    let slug = intent.trim().replace(' ', "_").to_lowercase();
    if slug.is_empty() {
        return Err(EaiError::protocol("Reflex intent cannot be empty"));
    }
    if slug.contains("..") || slug.contains('/') || slug.contains('\\') {
        return Err(EaiError::filesystem(
            "Reflex intent must not contain path separators or '..'",
        ));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(EaiError::filesystem(
            "Reflex intent must be alphanumeric/underscore/dash only after normalization",
        ));
    }
    Ok(slug)
}

/// A check run on the staged module before it is published.
type StagedCheck = dyn Fn(&Path) -> Result<(), String>;

/// A reflex slug for free text: lowercase ASCII alphanumerics, every other
/// run of characters collapsed to `_`, at most 48 chars; `None` when
/// nothing alphanumeric remains.
#[must_use]
pub fn slug_for(text: &str) -> Option<String> {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('_') && !slug.is_empty() {
            slug.push('_');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_matches('_').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// Where a reflex `<slug>.wasm` is published.
#[must_use]
pub fn reflex_path(slug: &str) -> std::path::PathBuf {
    susi_paths::SusiDirs::data_dir()
        .join("reflexes")
        .join(format!("{slug}.wasm"))
}

/// Probe input a model-written reflex must run on before it is published.
const PROBE_INPUT: &str = "susi-probe";

/// Result of [`ReflexSynthesizer::synthesize_capability`].
#[derive(Debug, Clone)]
pub struct ReflexSynthesis {
    /// Published `reflexes/<slug>.wasm`.
    pub path: String,
    /// `true`: model-written, compiled and run on a probe input.
    /// `false`: the signature-echo probe reflex (capability not implemented).
    pub functional: bool,
    /// Why the functional attempt failed, when `functional` is `false`.
    pub note: Option<String>,
}

/// First fenced code block (```rust / ```rs / ```) in `reply`.
fn extract_rust_block(reply: &str) -> Option<String> {
    let start = reply.find("```")?;
    let after = &reply[start + 3..];
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```")?;
    let code = body[..end].trim();
    (!code.is_empty()).then(|| code.to_string())
}

impl ReflexSynthesizer {
    /// Synthesizes and compiles a WASI reflex that can be hot-loaded by
    /// `ToolRegistry::execute_tool` (the `reflex_<name>` convention) without a
    /// daemon restart — the concrete mechanism behind roadmap.json's VC-200-002
    /// "Autonomous Trait Patching" vector. Output is derived from the reflex's
    /// runtime argument (FNV-1a signature), not a hardcoded constant, so two
    /// different calls are verifiably not just replaying the same canned value.
    ///
    /// Requires the `wasm32-wasip1` rustup target (`rustup target add
    /// wasm32-wasip1`); compilation failure is returned as an `Err`, never
    /// silently swallowed, so callers can tell a real patch from a missing
    /// toolchain component.
    pub fn synthesize_wasm_reflex(intent: &str, _workspace: &Path) -> EaiResult<String> {
        let slug = sanitize_reflex_slug(intent)?;
        let path = Self::compile_and_publish(&slug, &Self::generate_reflex_source(intent), None)?;
        Ok(path.to_string_lossy().to_string())
    }

    /// Close a capability gap for real when possible: ask the local model for
    /// a std-only program implementing `intent` (input = argv[1], result on
    /// stdout); publish it only if the source parses, compiles to
    /// `wasm32-wasip1`, and runs successfully on a probe input under the
    /// `susi-native` WASI sandbox. Otherwise fall back to the probe reflex,
    /// keeping why the functional attempt failed.
    ///
    /// # Errors
    /// Invalid intent slug, or both the functional and the probe build fail.
    pub fn synthesize_capability(intent: &str, workspace: &Path) -> EaiResult<ReflexSynthesis> {
        Self::synthesize_capability_for(intent, intent, workspace)
    }

    /// [`synthesize_capability`](Self::synthesize_capability) with a reflex
    /// `name` (must be a valid slug) separate from the free-text
    /// `description` the model implements -- used when the gap is a
    /// recurring mission intent rather than a missing tool name.
    ///
    /// # Errors
    /// Invalid `name`, or both the functional and the probe build fail.
    pub fn synthesize_capability_for(
        name: &str,
        description: &str,
        workspace: &Path,
    ) -> EaiResult<ReflexSynthesis> {
        let intent = name;
        let slug = sanitize_reflex_slug(name)?;
        let functional = Self::model_reflex_source(description, workspace).and_then(|src| {
            let verify = |staged: &Path| -> Result<(), String> {
                match crate::susi_native::WasmHost::execute_reflex(staged, PROBE_INPUT) {
                    Ok(out) if !out.trim().is_empty() => Ok(()),
                    Ok(_) => Err("reflex produced no output on the probe input".into()),
                    Err(e) => Err(format!("reflex failed on the probe input: {e}")),
                }
            };
            Self::compile_and_publish(&slug, &src, Some(&verify))
        });
        match functional {
            Ok(path) => Ok(ReflexSynthesis {
                path: path.to_string_lossy().to_string(),
                functional: true,
                note: None,
            }),
            Err(why) => {
                let path = Self::synthesize_wasm_reflex(intent, workspace)?;
                Ok(ReflexSynthesis {
                    path,
                    functional: false,
                    note: Some(why.to_string()),
                })
            }
        }
    }

    /// Model-written reflex source for `intent`: the first fenced Rust block
    /// of the reply, required to parse and to define `fn main`.
    fn model_reflex_source(intent: &str, workspace: &Path) -> EaiResult<String> {
        let prompt = format!(
            "Write a complete, self-contained Rust program using only the standard library \
             (no external crates, no networking, no filesystem access) for the \
             wasm32-wasip1 target that implements: {intent}\n\
             Read the input from the first command-line argument \
             (std::env::args().nth(1)) and print the result to stdout. \
             Reply with the program in a single ```rust code block."
        );
        let reply = crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
            &prompt, workspace,
        );
        let source = extract_rust_block(&reply)
            .ok_or_else(|| EaiError::inference("model reply contained no Rust code block"))?;
        if !susi_vendor_syn::is_valid_rust(&source) || !source.contains("fn main") {
            return Err(EaiError::inference(
                "model-written reflex is not a valid Rust program with fn main",
            ));
        }
        Ok(source)
    }

    /// Write `source`, compile it to a private staging name, optionally run
    /// `verify` on the staged module, then rename it into the live
    /// `reflexes/<slug>.wasm` name. `ToolRegistry` executes every
    /// `reflexes/*.wasm`, so nothing unverified or half-written is ever
    /// visible under a live name.
    fn compile_and_publish(
        slug: &str,
        source: &str,
        verify: Option<&StagedCheck>,
    ) -> EaiResult<std::path::PathBuf> {
        let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
        let _ = fs::create_dir_all(&reflex_dir);
        let wasm_src = reflex_dir.join(format!("{slug}.rs"));
        fs::write(&wasm_src, source)?;
        let wasm_out = reflex_dir.join(format!("{slug}.wasm"));
        let staged = reflex_dir.join(format!("{slug}.wasm.{}.tmp", std::process::id()));
        // Mandate 42: non-UTF8 paths are errors, never panics.
        let staged_str = staged.to_str().ok_or_else(|| {
            EaiError::process(format!(
                "WASM output path is not valid UTF-8: {}",
                wasm_out.display()
            ))
        })?;
        let src_str = wasm_src.to_str().ok_or_else(|| {
            EaiError::process(format!(
                "WASM source path is not valid UTF-8: {}",
                wasm_src.display()
            ))
        })?;
        let output = Command::new("rustc")
            .args(["--target", "wasm32-wasip1", "-O", "-o", staged_str, src_str])
            .output()
            .map_err(|e| {
                let _ = fs::remove_file(&staged);
                EaiError::process(format!("rustc invocation failed: {e}"))
            })?;
        if !output.status.success() {
            let _ = fs::remove_file(&staged);
            return Err(EaiError::process(format!(
                "WASM compilation failed (is the wasm32-wasip1 rustup target installed? \
                 `rustup target add wasm32-wasip1`): {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if let Some(verify) = verify {
            if let Err(why) = verify(&staged) {
                let _ = fs::remove_file(&staged);
                return Err(EaiError::process(why));
            }
        }
        if let Err(e) = fs::rename(&staged, &wasm_out) {
            let _ = fs::remove_file(&staged);
            return Err(EaiError::filesystem(format!(
                "publish reflex {}: {e}",
                wasm_out.display()
            )));
        }
        Ok(wasm_out)
    }

    /// The one user-facing report of a capability-gap attempt (Mandate 1):
    /// a model-written reflex is reported as compiled and run on a probe
    /// input — its correctness for the intent is not verified — and a probe
    /// reflex is reported as not implementing the capability at all.
    pub fn gap_report(name: &str, outcome: &EaiResult<ReflexSynthesis>) -> String {
        match outcome {
            Ok(r) if r.functional => format!(
                "[HOT_PATCH] Synthesized a model-written WASI reflex for '{name}' at {} \
                 (callable as 'reflex_{name}'). It compiled and ran on a probe input; its \
                 correctness for '{name}' is not verified.",
                r.path
            ),
            Ok(r) => format!(
                "[HOT_PATCH] Compiled a probe WASI reflex for '{name}' at {} \
                 (callable as 'reflex_{name}'). It verifies the hot-load path and returns an \
                 input signature only; it does not implement '{name}', so the capability \
                 gap remains open.{}",
                r.path,
                r.note
                    .as_ref()
                    .map(|n| format!(" Functional synthesis failed: {n}"))
                    .unwrap_or_default()
            ),
            Err(e) => format!(
                "[CAPABILITY_GAP] '{name}' unresolved: no registry match, no installable \
                 package, and reflex synthesis failed ({e})."
            ),
        }
    }

    fn generate_reflex_source(intent: &str) -> String {
        let intent_literal = format!("{:?}", intent);
        format!(
            "// SUSI Synthesized WASI Reflex\n\
            // Autonomous hot-patch generated to close a capability gap without a\n\
            // daemon restart. Signature is derived from the runtime argument, so\n\
            // output is verifiably input-dependent rather than a hardcoded stub.\n\
            fn main() {{\n\
                const INTENT: &str = {intent_literal};\n\
                let arg = std::env::args().nth(1).unwrap_or_default();\n\
                let mut hash: u64 = 0xcbf29ce484222325;\n\
                for byte in arg.bytes().chain(INTENT.bytes()) {{\n\
                    hash ^= byte as u64;\n\
                    hash = hash.wrapping_mul(0x100000001b3);\n\
                }}\n\
                println!(\"[REFLEX:{{}}] input={{}} signature={{:016x}}\", INTENT, arg, hash);\n\
            }}\n",
            intent_literal = intent_literal
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_reflex_source_is_valid_rust_and_input_dependent() {
        let a = ReflexSynthesizer::generate_reflex_source("bloat_audit");
        let b = ReflexSynthesizer::generate_reflex_source("sovereign_dashboard");
        assert_ne!(
            a, b,
            "different intents must synthesize different reflex logic"
        );

        for src in [&a, &b] {
            assert!(
                susi_vendor_syn::is_valid_rust(src),
                "synthesized reflex source failed to parse as valid Rust:\n{}",
                src
            );
            assert!(
                src.contains("fn main()"),
                "reflex must define fn main() so WasmHost finds a _start entry point"
            );
        }
    }

    #[test]
    fn test_wasm_reflex_source_escapes_hostile_intent() {
        // An intent containing a quote must not break the embedded string literal.
        let src = ReflexSynthesizer::generate_reflex_source(r#"weird "intent" with quotes"#);
        assert!(
            susi_vendor_syn::is_valid_rust(&src),
            "hostile intent text broke the generated source:\n{}",
            src
        );
    }

    #[test]
    fn slug_for_normalizes_free_text() {
        assert_eq!(
            slug_for("Summarize the logs!").unwrap(),
            "summarize_the_logs"
        );
        assert_eq!(slug_for("  --a  b--  ").unwrap(), "a_b");
        assert!(slug_for("!!!").is_none());
        assert!(slug_for(&"x".repeat(100)).unwrap().len() <= 48);
    }

    #[test]
    fn extracts_first_fenced_block() {
        let reply = "Here:\n```rust\nfn main() { println!(\"x\"); }\n```\nDone.";
        assert_eq!(
            extract_rust_block(reply).unwrap(),
            "fn main() { println!(\"x\"); }"
        );
        assert!(extract_rust_block("no code here").is_none());
        assert!(extract_rust_block("```rust\n```").is_none());
    }

    #[test]
    fn gap_report_never_claims_a_probe_implements_the_capability() {
        let probe = Ok(ReflexSynthesis {
            path: "/r/x.wasm".into(),
            functional: false,
            note: Some("model reply contained no Rust code block".into()),
        });
        let text = ReflexSynthesizer::gap_report("x", &probe);
        assert!(text.contains("does not implement 'x'") && text.contains("remains open"));
        assert!(text.contains("no Rust code block"));
        let real = Ok(ReflexSynthesis {
            path: "/r/x.wasm".into(),
            functional: true,
            note: None,
        });
        let text = ReflexSynthesizer::gap_report("x", &real);
        assert!(text.contains("correctness for 'x' is not verified"));
    }

    #[test]
    #[ignore = "requires the wasm32-wasip1 rustup target; writes under the data dir"]
    fn test_wasm_reflex_publishes_atomically_and_leaves_no_staging_file() {
        let slug = format!("zz_atomic_probe_{}", std::process::id());
        let out = ReflexSynthesizer::synthesize_wasm_reflex(&slug, Path::new(".")).unwrap();
        let dir = susi_paths::SusiDirs::data_dir().join("reflexes");
        assert!(out.ends_with(&format!("{slug}.wasm")));
        assert_eq!(&std::fs::read(&out).unwrap()[..4], b"\0asm");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&slug))
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(dir.join(format!("{slug}.rs")));
    }

    #[test]
    #[ignore = "requires the standalone susi-native service"]
    fn test_wasm_reflex_hot_patch_end_to_end() {
        // Best-effort: actually compiles and hot-loads the reflex if the
        // wasm32-wasip1 rustup target is installed on this machine. If it isn't,
        // this honestly reports that instead of claiming success it didn't earn -
        // this environment does not have the target installed (verified via
        // `rustc --print target-list` + a direct compile attempt during
        // development), so this path is expected to be skipped in CI/sandbox.
        let intent = format!("test_hot_patch_reflex_{}", std::process::id());
        match ReflexSynthesizer::synthesize_wasm_reflex(&intent, Path::new(".")) {
            Ok(wasm_path) => {
                let _home = susi_paths::SusiDirs::home_dir();
                let result =
                    crate::susi_native::WasmHost::execute_reflex(Path::new(&wasm_path), "hello");
                let _ = std::fs::remove_file(&wasm_path);
                let _ = std::fs::remove_file(
                    susi_paths::SusiDirs::data_dir()
                        .join("reflexes")
                        .join(format!("{}.rs", intent)),
                );
                let output = result.expect("compiled reflex must execute successfully");
                assert!(output.contains("input=hello"));
            }
            Err(e) => {
                eprintln!(
                    "[SKIP] wasm32-wasip1 toolchain unavailable, hot-patch not exercised end-to-end: {}",
                    e
                );
            }
        }
    }
}
