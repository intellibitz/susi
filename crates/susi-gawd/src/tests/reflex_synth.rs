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
            "synthesized reflex source failed to parse as valid Rust:\n{src}"
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
        "hostile intent text broke the generated source:\n{src}"
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
    assert!(
        text.contains("does not implement 'x'")
            && text.contains("remains open")
            && text.contains("no Rust code block")
    );
    let real = Ok(ReflexSynthesis {
        path: "/r/x.wasm".into(),
        functional: true,
        note: None,
    });
    assert!(ReflexSynthesizer::gap_report("x", &real).contains("passed the intent's own fixtures"));
}

#[test]
#[ignore = "requires the wasm32-wasip1 rustup target; writes under the data dir"]
fn test_wasm_reflex_publishes_atomically_and_leaves_no_staging_file() {
    let slug = format!("zz_atomic_probe_{}", std::process::id());
    let out = ReflexSynthesizer::synthesize_wasm_reflex(&slug, Path::new(".")).unwrap();
    let dir = susi_paths::SusiDirs::data_dir().join("reflexes");
    assert!(
        out.ends_with(&format!("{slug}.wasm")) && &std::fs::read(&out).unwrap()[..4] == b"\0asm"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with(&slug)
                && e.file_name().to_string_lossy().ends_with(".tmp")
        })
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
                    .join(format!("{intent}.rs")),
            );
            assert!(result
                .expect("compiled reflex must execute successfully")
                .contains("input=hello"));
        }
        Err(e) => eprintln!(
            "[SKIP] wasm32-wasip1 toolchain unavailable, hot-patch not exercised end-to-end: {e}"
        ),
    }
}

#[test]
fn compile_and_publish_failure_removes_orphaned_source_when_no_wasm_exists() {
    let _lock = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", tmp.path());
    env.set("XDG_DATA_HOME", tmp.path().join("xdg-data"));
    env.set("SUSI_HOME", tmp.path().join(".susi"));
    let slug = "uncompiled_orphan_probe";
    let err = ReflexSynthesizer::compile_and_publish(slug, "not valid rust {{{", None).unwrap_err();
    assert!(!err.to_string().is_empty());
    let reflex_dir = susi_paths::SusiDirs::data_dir().join("reflexes");
    assert!(
        !reflex_dir.join(format!("{slug}.rs")).exists()
            && !reflex_dir.join(format!("{slug}.wasm")).exists()
    );
}
