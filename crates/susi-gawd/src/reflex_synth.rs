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
        let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
        let _ = fs::create_dir_all(&reflex_dir);
        let slug = sanitize_reflex_slug(intent)?;
        let wasm_src = reflex_dir.join(format!("{}.rs", slug));

        let code = Self::generate_reflex_source(intent);
        fs::write(&wasm_src, code)?;

        let wasm_out = reflex_dir.join(format!("{}.wasm", slug));
        // rustc writes its output in place, and ToolRegistry lists and
        // executes every `reflexes/*.wasm` that exists: compiling straight
        // to the live name let a concurrent call run a half-written module
        // (and a failed rebuild could leave a truncated one behind). Build
        // to a private non-`.wasm` name and rename into place on success.
        let staged = reflex_dir.join(format!("{}.wasm.{}.tmp", slug, std::process::id()));
        // Mandate 42: `to_str()` is `None` on a non-UTF8 path (possible, if
        // rare, under an unusual locale/HOME) - propagate that as a real
        // error instead of panicking, consistent with every other failure
        // mode this function already returns as `Err` below.
        let wasm_out_str = staged.to_str().ok_or_else(|| {
            EaiError::process(format!(
                "WASM output path is not valid UTF-8: {}",
                wasm_out.display()
            ))
        })?;
        let wasm_src_str = wasm_src.to_str().ok_or_else(|| {
            EaiError::process(format!(
                "WASM source path is not valid UTF-8: {}",
                wasm_src.display()
            ))
        })?;

        let build = Command::new("rustc")
            .args([
                "--target",
                "wasm32-wasip1",
                "-O",
                "-o",
                wasm_out_str,
                wasm_src_str,
            ])
            .output();

        let output = match build {
            Ok(output) => output,
            Err(e) => {
                let _ = fs::remove_file(&staged);
                return Err(EaiError::process(format!("rustc invocation failed: {}", e)));
            }
        };
        if !output.status.success() {
            let _ = fs::remove_file(&staged);
            return Err(EaiError::process(format!(
                "WASM compilation failed (is the wasm32-wasip1 rustup target installed? \
                 `rustup target add wasm32-wasip1`): {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if let Err(e) = fs::rename(&staged, &wasm_out) {
            let _ = fs::remove_file(&staged);
            return Err(EaiError::filesystem(format!(
                "publish reflex {}: {e}",
                wasm_out.display()
            )));
        }
        Ok(wasm_out.to_string_lossy().to_string())
    }

    /// Generates the reflex's WASI source. Split out from `synthesize_wasm_reflex`
    /// so the generated code's syntactic validity and input-dependence can be unit
    /// tested without requiring the wasm32-wasip1 toolchain to be installed.
    /// `intent` is embedded via `{:?}` (a proper escaped Rust string literal), so
    /// arbitrary intent text — including quotes — can never produce broken source.
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
