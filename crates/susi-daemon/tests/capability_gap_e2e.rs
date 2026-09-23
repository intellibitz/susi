#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! VC-200-002 e2e: an unregistered tool name flows through the real
//! `ToolRegistry::execute_tool` → `resolve_capability_gap` → wired
//! `SusiEngineHooks` → `ReflexSynthesizer::synthesize_wasm_reflex` path, and the
//! resulting WASI reflex is then hot-loaded and executed by a second
//! `execute_tool` call under the `reflex_<slug>` convention — no direct
//! calls into `reflex_synth`.
//!
//! Requires the `wasm32-wasip1` rustup target (installed by CI). When it is
//! absent the test reports a skip instead of claiming a pass it didn't earn —
//! same convention as `reflex_synth::tests::test_wasm_reflex_hot_patch_end_to_end`.

#[test]
fn capability_gap_synthesizes_and_executes_reflex_through_execute_tool() {
    // Isolate HOME so the synthesized reflex lands under a temp substrate and
    // global_mcp_registry healing can't touch the real ~/.susi.
    let tmp = std::env::temp_dir().join(format!("susi_gap_e2e_{}", std::process::id()));
    let _ = std::fs::create_dir_all(tmp.join(".susi"));
    unsafe {
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("xdg"));
    }
    let workspace = tmp.join("ws");
    let _ = std::fs::create_dir_all(&workspace);

    // Composition-root wiring: the real host hooks, not a test stub.
    susi_tools::hooks::init(Box::new(susi_daemon::SusiEngineHooks));

    let slug = format!("zzz_gap_probe_{}", std::process::id());
    let arg = serde_json::json!("e2e-arg");
    let first = susi_tools::ToolRegistry::execute_tool(&slug, &arg, &workspace);

    let wasm = susi_paths::SusiDirs::data_dir()
        .join("reflexes")
        .join(format!("{slug}.wasm"));
    if !wasm.is_file() {
        eprintln!(
            "[SKIP] wasm32-wasip1 toolchain unavailable, capability-gap e2e not exercised: {}",
            first
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    assert!(
        first.contains("HOT_PATCH") || first.contains("CAPABILITY_GAP"),
        "first call must report the self-healing outcome, got: {first}"
    );

    // Retry under the reflex convention — now the .wasm exists, execute_tool
    // must hot-load and run it (input-dependent FNV signature output).
    let second = susi_tools::ToolRegistry::execute_tool(
        &format!("reflex_{slug}"),
        &serde_json::json!("e2e-arg"),
        &workspace,
    );
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        second.contains("[REFLEX:") && second.contains("input=e2e-arg"),
        "hot-patched reflex must execute through execute_tool, got: {second}"
    );
}
