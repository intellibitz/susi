use super::{contains_phrase, SusiAdmin};

fn isolated_home() -> (
    std::sync::MutexGuard<'static, ()>,
    susi_paths::tempfile::TempDir,
    susi_paths::test_env::EnvGuard,
) {
    let guard = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", home.path());
    env.set("SUSI_HOME", home.path());
    (guard, home, env)
}

#[test]
fn first_goal_evidence_labels_do_not_infer() {
    const CHILD: &str = "SUSI_INGEST_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "compliance::intent_classify_tests::first_goal_evidence_labels_do_not_infer",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let temporary_home = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", temporary_home.path());
    env.set("SUSI_HOME", temporary_home.path());
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct PulseProbe(Arc<AtomicUsize>);
    impl PlaneHandler for PulseProbe {
        fn handle(&self, _: &str, _: serde_json::Value) -> Result<serde_json::Value, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"text":"motion"}))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    PlaneBus::global().register(
        topics::GEMI_PULSE_REASON,
        Arc::new(PulseProbe(calls.clone())),
    );
    let workspace = susi_paths::SusiDirs::data_dir();
    for goal in [
        "Read step1-note.txt and tell me its launch code",
        "Compute 17 + 25. Answer with only the number.",
    ] {
        assert_eq!(
            SusiAdmin::classify_natural_intent(&workspace, goal).0,
            "[QUERY]"
        );
    }
    assert_eq!(
        SusiAdmin::classify_natural_intent(&workspace, "draft a promotion plan").0,
        "[MISSION]"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "evidence ingestion must not perform inference"
    );
}

#[test]
fn keywords_match_whole_words_only() {
    assert!(contains_phrase(
        "rework the architecture now",
        "architecture"
    ));
    assert!(contains_phrase("motion", "motion"));
    assert!(contains_phrase("run who am i here", "who am i"));
    assert!(!contains_phrase("draft a promotion plan", "motion"));
    assert!(!contains_phrase("track user emotions", "motion"));
}

#[test]
fn short_intents_are_ingested_redacted_and_exact_repeats_deduped() {
    let (_guard, temporary_home, _env) = isolated_home();
    let ws = temporary_home.path().join("workspace");
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    std::fs::write(
        ws.join(".susi/evidence.json"),
        r#"{"entries":[{"id":"EV-2022927-001","milestone":"add retries to the downloader","proof":"VERIFIED"}]}"#,
    )
    .unwrap();
    let count = || {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".susi/evidence.json")).unwrap())
                .unwrap();
        v["entries"].as_array().unwrap().len()
    };
    SusiAdmin::ingest_natural_intent(&ws, "add retries").unwrap();
    assert_eq!(
        count(),
        2,
        "a substring of an existing milestone is not a duplicate"
    );
    let again = SusiAdmin::ingest_natural_intent(&ws, "Add Retries").unwrap();
    assert!(again.contains("already present") && count() == 2, "{again}");
    SusiAdmin::ingest_natural_intent(&ws, "rotate ghp_ingestProbe999 now").unwrap();
    let text = std::fs::read_to_string(ws.join(".susi/evidence.json")).unwrap();
    assert!(!text.contains("ghp_ingestProbe999"), "{text}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn config_lists_drive_classification() {
    let (_guard, _home, _env) = isolated_home();
    let ws = std::env::temp_dir();
    assert_eq!(
        SusiAdmin::classify_natural_intent(&ws, "refactor the architecture").0,
        "[MOTION]"
    );
    assert_eq!(
        SusiAdmin::classify_natural_intent(&ws, "status").0,
        "[QUERY]"
    );
    assert_ne!(
        SusiAdmin::classify_natural_intent(&ws, "draft a promotion plan").0,
        "[MOTION]"
    );
}

#[test]
fn global_susi_dir_and_contains_phrase_handle_empty_and_boundary_cases() {
    assert!(!contains_phrase("any text", ""));
    assert!(contains_phrase("(status)", "status"));
    assert!(!contains_phrase("statuses", "status"));
    // Both sides resolve the home from the environment, which the
    // HOME-swapping tests in this module change under `ENV_LOCK`; a swap
    // landing between the two reads made them differ.
    let _guard = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(
        SusiAdmin::get_global_susi_dir(),
        susi_paths::SusiDirs::config_dir()
    );
}

#[test]
fn audit_compliance_ignores_substring_sk_words_and_cfg_test_modules() {
    let (_guard, temporary_home, _env) = isolated_home();
    let ws = temporary_home.path().join("clean-audit-ws");
    std::fs::create_dir_all(ws.join("src/cli")).unwrap();
    std::fs::write(ws.join(".gitignore"), ".susi/\n").unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"clean-audit-ws\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("src/cli/defs.rs"),
        "pub const CMD: &str = \"task-queue\";\npub fn risk_eval() {}\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("src/cli_json.rs"),
        "pub fn ok() {}\n\n#[cfg(test)]\n// comment between cfg and mod\n#[allow(clippy::unwrap_used)]\nmod inline_tests {\n    #[test]\n    fn fixture() { let _ = format!(\"sk-{}\", \"x\"); }\n}\n",
    )
    .unwrap();
    let report = SusiAdmin::audit_compliance(&ws, None).expect("clean workspace audit");
    assert!(
        report.contains("- [PASS] Security: No hardcoded secrets detected."),
        "{report}"
    );
}

#[test]
fn audit_compliance_flags_real_secret_prefix_outside_cfg_test() {
    let (_guard, temporary_home, _env) = isolated_home();
    let ws = temporary_home.path().join("dirty-audit-ws");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join(".gitignore"), ".susi/\n").unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"dirty-audit-ws\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("src/leak.rs"),
        "pub const KEY: &str = \"sk-liveprod1234567890\";\n",
    )
    .unwrap();
    let msg = SusiAdmin::audit_compliance(&ws, None)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("Potential secret matching 'sk-'"), "{msg}");
}

#[test]
fn source_evidence_is_ignored_and_new_runtime_history_starts_empty() {
    let (_guard, temporary_home, _env) = isolated_home();
    let ws = temporary_home.path().join("source-ledger-workspace");
    std::fs::create_dir_all(ws.join(".agents")).unwrap();
    let source = ws.join(".agents/evidence.json");
    let historical =
        r#"{"entries":[{"id":"EV-OLD-001","milestone":"archived assertion","proof":"VERIFIED"}]}"#;
    std::fs::write(&source, historical).unwrap();
    SusiAdmin::ingest_natural_intent(&ws, "rotate ghp_historyProbe999 now").unwrap();
    assert_eq!(std::fs::read_to_string(&source).unwrap(), historical);
    let local = susi_paths::workspace_state_dir(&ws).join("evidence.json");
    let raw = std::fs::read_to_string(&local).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc["schema"], "susi/evidence/v1");
    let entries = doc["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "old source assertions are not seeded");
    assert_eq!(entries[0]["proof"], "STAGED");
    assert!(!raw.contains("ghp_historyProbe999"));
    assert!(!raw.contains("archived assertion"));
    let again = SusiAdmin::ingest_natural_intent(&ws, "ROTATE ghp_historyProbe999 NOW").unwrap();
    assert!(again.contains("already present"));
    assert_eq!(std::fs::read_to_string(&local).unwrap(), raw);
    assert_eq!(std::fs::read_to_string(&source).unwrap(), historical);
}

#[test]
fn malformed_local_history_refuses_without_overwriting_or_source_fallback() {
    let (_guard, temporary_home, _env) = isolated_home();
    let ws = temporary_home.path().join("invalid-local-history");
    std::fs::create_dir_all(ws.join(".agents")).unwrap();
    std::fs::create_dir_all(susi_paths::workspace_state_dir(&ws)).unwrap();
    let source = ws.join(".agents/evidence.json");
    std::fs::write(&source, r#"{"entries":[]}"#).unwrap();
    let local = susi_paths::workspace_state_dir(&ws).join("evidence.json");
    std::fs::write(&local, "invalid local history").unwrap();
    let error = SusiAdmin::ingest_natural_intent(&ws, "status").unwrap_err();
    assert!(error.to_string().contains("invalid JSON"));
    assert_eq!(
        std::fs::read_to_string(local).unwrap(),
        "invalid local history"
    );
    assert_eq!(
        std::fs::read_to_string(source).unwrap(),
        r#"{"entries":[]}"#
    );
}
