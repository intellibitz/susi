#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! End-to-end distill stage of the learning loop, across the plane bus:
//! production staging writer → `ReflexTrainer` audit (claim, GEMI training
//! over the bus, held-out gate, publication, cycle log) → Tier-0 serving
//! through `SusiPulse`. Each piece has unit tests in its own crate; this
//! proves they compose.

#![allow(missing_docs)] // integration test crate: no public API to document

use std::path::PathBuf;
use std::sync::Once;

fn test_home() -> PathBuf {
    std::env::temp_dir().join(format!("susi_distill_loop_{}", std::process::id()))
}

fn wire_test_substrate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Isolate every substrate root before wiring: training publishes
        // Tier-0 weights under the config dir, which must not be the host's.
        let home = test_home();
        std::fs::create_dir_all(&home).unwrap();
        // Instance knobs outrank HOME/XDG: a dev-instance launch must not
        // redirect this process-lifetime substrate (or its ports).
        std::env::remove_var("SUSI_HOME");
        std::env::remove_var("SUSI_PORT_OFFSET");
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
            std::env::set_var("XDG_CONFIG_HOME", home.join("xdg-config"));
            std::env::set_var("XDG_DATA_HOME", home.join("xdg-data"));
            std::env::set_var("SUSI_TEST_MOCK_INFERENCE", "true");
        }
        susi_daemon::composition::wire_plane_bus();
        susi_tools::hooks::init(Box::new(susi_daemon::SusiEngineHooks));
    });
}

fn stage(workspace: &std::path::Path, intent: &str, action: &str) {
    susi_gawd::pkb::ProtocolKnowledgeBase::stage_distillation_pair(
        intent,
        action,
        workspace,
        Some(serde_json::json!({ "outcome": "SUCCESS", "route": "fast-path" })),
    )
    .unwrap();
}

#[test]
fn staged_successes_train_publish_and_serve_a_tier0_reflex() {
    wire_test_substrate();
    let workspace = test_home().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    // Enough successful samples to cross the default training threshold
    // (50), plus failures that must never teach anything.
    let listings = ["list files in src", "ls the folder", "list the directory"];
    let statuses = [
        "check system status",
        "show health report",
        "system status check",
    ];
    for round in 0..9 {
        for intent in listings {
            stage(&workspace, &format!("{intent} {round}"), "list_directory");
        }
        for intent in statuses {
            stage(&workspace, &format!("{intent} {round}"), "status");
        }
    }
    susi_gawd::pkb::ProtocolKnowledgeBase::stage_distillation_pair(
        "list files in src",
        "status",
        &workspace,
        Some(serde_json::json!({ "outcome": "FAILED" })),
    )
    .unwrap();

    let report =
        susi_gawd::reflex_trainer::ReflexTrainer::audit_distillation_state(&workspace).unwrap();
    assert!(report.contains("Native distillation complete"), "{report}");

    // The claim was consumed and the cycle logged as published.
    let staged = workspace.join(".susi/distillation_staged.jsonl");
    assert!(!staged.exists() || std::fs::read_to_string(&staged).unwrap().is_empty());
    let summary = susi_gawd::reflex_trainer::distillation_summary(&workspace);
    assert_eq!(summary["published"], 1, "{summary}");

    // The published checkpoint itself (in the isolated config dir) makes
    // the prediction — not a Tier-1 fallback.
    let model =
        susi_gemi::engines::alpha::SusiAlphaModel::load(&susi_paths::SusiDirs::config_dir())
            .unwrap();
    assert_eq!(
        model.predict_intent("list files in src").unwrap(),
        "ACTION: list_directory"
    );
    assert!(model.support("list files in src").unwrap() > 0.9);

    // Tier-0 serves the familiar prompt from the freshly published weights
    // (pulse rewrites list_directory to carry the workspace path).
    let served = susi_gemi::pulse::SusiPulse::reason("list files in src", &workspace).unwrap();
    assert!(
        served.starts_with("ACTION: list_directory"),
        "familiar prompt not served by Tier-0: {served}"
    );

    // Through the plane bus, the ordinary stream serves the reflex — which is
    // why synthesis callers use `generate_reasoning_stream_deep`. (Asserting
    // the deep side end to end costs ~60s of provider discovery under mock
    // inference, so it is not exercised here.)
    let reflexed = susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_stream(
        "list files in src",
        &workspace,
        &|_| {},
    );
    assert!(reflexed.starts_with("ACTION: list_directory"), "{reflexed}");
}

#[test]
fn an_untrainable_claim_is_retired_not_retried_forever() {
    wire_test_substrate();
    let workspace = test_home().join("workspace-untrainable");
    std::fs::create_dir_all(&workspace).unwrap();
    // Past the threshold, but no line can ever teach: the action is not a
    // capability, so every record is skipped.
    for i in 0..55 {
        stage(
            &workspace,
            &format!("frobnicate the widget {i}"),
            "no_such_capability",
        );
    }
    let report =
        susi_gawd::reflex_trainer::ReflexTrainer::audit_distillation_state(&workspace).unwrap();
    assert!(report.contains("Retired 55 untrainable"), "{report}");

    let staged = workspace.join(".susi/distillation_staged.jsonl");
    assert!(
        !staged.exists() || std::fs::read_to_string(&staged).unwrap().is_empty(),
        "an untrainable claim must not be restored"
    );
    let summary = susi_gawd::reflex_trainer::distillation_summary(&workspace);
    assert_eq!(summary["untrainable"], 1, "{summary}");
    assert_eq!(summary["error"], 0, "{summary}");
}
