use super::*;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_temp_workspace(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "susi_gawd_native_test_{}_{}_{}",
        label,
        std::process::id(),
        id
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn is_reflex_query_matches_positive_cases_case_insensitively() {
    let positives = [
        "ls",
        "  LS  ",
        "ls -la",
        "LS /tmp",
        "dir",
        "  DIR\n",
        "who am i",
        "WHO AM I",
        "whoami",
        "  WhoAmI  ",
        "tell me who am i",
        "please run whoami now",
        "status",
        "  STATUS ",
        "identity",
        "IDENTITY",
        "version",
        "VERSION",
        "models",
        "MODELS",
        "help",
        "HELP",
        "health",
        "HEALTH",
        "admin pulse: sync",
        "  Admin Pulse: audit ",
        "admin pulse",
    ];
    for query in positives {
        assert!(
            is_reflex_query(query),
            "expected '{query}' to be recognized as a reflex query"
        );
    }
}

#[test]
fn is_reflex_query_rejects_non_reflex_goals() {
    let negatives = [
        "",
        "   ",
        "implement feature X",
        "list_models",
        "directory layout",
        "healthy cluster",
        "status_report",
        "versioning strategy",
        "helpful guide",
        "identities",
        "lsblk",
        "trigger admin pulse",
    ];
    for query in negatives {
        assert!(
            !is_reflex_query(query),
            "expected '{query}' NOT to be recognized as a reflex query"
        );
    }
}

#[test]
fn has_valid_local_weights_and_runtime_readiness_edge_cases() {
    // Non-array JSON shapes must return false
    assert!(!has_valid_local_weights(&json!({"models": []})));
    assert!(!has_valid_local_weights(&json!("invalid")));
    assert!(!has_valid_local_weights(&json!(42)));

    // Non-.gguf extensions or non-boolean flags must return false
    assert!(!has_valid_local_weights(&json!([
        {"path": "/models/weights.safetensors", "is_valid_gguf": true},
        {"path": "/models/weights.bin", "is_valid_gguf": true},
        {"path": "/models/no_ext", "is_valid_gguf": true},
        {"path": "/models/valid.gguf", "is_valid_gguf": "true"},
        {"path": 12345, "is_valid_gguf": true}
    ])));

    // Mixed array succeeds if at least one entry has is_valid_gguf == true and .gguf extension
    assert!(has_valid_local_weights(&json!([
        {"path": "/models/broken.gguf", "is_valid_gguf": false},
        {"path": "/models/other.safetensors", "is_valid_gguf": true},
        {"path": "/models/ready.gguf", "is_valid_gguf": true}
    ])));

    // runtime_readiness edge cases: leading whitespace before [NO_LOCAL_MODEL] still fails
    let leading_ws = "   \n\t[NO_LOCAL_MODEL] No model installed.";
    assert!(runtime_readiness(leading_ws).is_err());

    // [NO_LOCAL_MODEL] not at start is treated as normal message
    let mid_tag = "Status note: [NO_LOCAL_MODEL] was resolved";
    assert_eq!(runtime_readiness(mid_tag).unwrap(), mid_tag);
    assert_eq!(runtime_readiness("").unwrap(), "");
}

#[test]
fn context_agent_project_markers_and_top_level_scan() {
    let ws = unique_temp_workspace("context");

    // Empty workspace has no markers and (0, 0) counts
    assert!(detect_project_markers(&ws).is_empty());
    assert_eq!(scan_workspace_top_level(&ws), (0, 0));

    // Create markers and top-level files/directories
    fs::write(ws.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(ws.join("package.json"), "{}").unwrap();
    fs::write(ws.join("README.md"), "# Demo").unwrap();
    fs::create_dir(ws.join(".git")).unwrap();
    fs::create_dir(ws.join("src")).unwrap();
    // Nested file should not inflate top-level counts
    fs::write(ws.join("src").join("lib.rs"), "pub fn demo() {}").unwrap();

    let markers = detect_project_markers(&ws);
    assert_eq!(markers, vec!["Cargo.toml", "package.json", ".git"]);

    let (files, dirs) = scan_workspace_top_level(&ws);
    assert_eq!(files, 3);
    assert_eq!(dirs, 2);

    let bb = MissionBlackboard::default();
    let report = ContextAgent.execute("analyze workspace", &ws, &bb).unwrap();
    assert!(report.contains("3 top-level file(s), 2 top-level subdirectory(ies)"));
    assert!(report.contains("Cargo.toml, package.json, .git"));
    assert!(bb.contains_key("ContextAgent"));

    // Non-existent path returns (0, 0) and no markers
    let missing = ws.join("does_not_exist");
    assert_eq!(scan_workspace_top_level(&missing), (0, 0));
    assert!(detect_project_markers(&missing).is_empty());

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn epistemic_auditor_path_extraction_and_grounding() {
    let ws = unique_temp_workspace("epistemic");
    fs::create_dir_all(ws.join("src")).unwrap();
    fs::write(ws.join("Cargo.toml"), "").unwrap();
    fs::write(ws.join("src").join("lib.rs"), "").unwrap();

    let text = "Checked `Cargo.toml`, (src/lib.rs:42), [README.md.], and \"scripts/run.sh\"; ignored main.py and notes.txt.";
    let extracted = extract_candidate_file_paths(text);
    assert_eq!(
        extracted,
        vec!["Cargo.toml", "src/lib.rs", "README.md", "scripts/run.sh"]
    );

    let (count, hallucinated) = audit_claim_grounding(text, &ws);
    assert_eq!(count, 4);
    assert_eq!(hallucinated, vec!["README.md", "scripts/run.sh"]);

    // 1. Grounded claims pass
    let bb_ok = MissionBlackboard::default();
    bb_ok.insert(
        "WorkerA".to_string(),
        "Verified `Cargo.toml` and `src/lib.rs:10`.".to_string(),
    );
    let res_ok = EpistemicAuditorAgent.execute("audit", &ws, &bb_ok).unwrap();
    assert!(res_ok.contains("FILE REFERENCES VERIFIED"));

    // 2. Hallucinated file path is rejected
    let bb_hallucinated = MissionBlackboard::default();
    bb_hallucinated.insert(
        "WorkerB".to_string(),
        "Updated `src/nonexistent_module.rs` in the workspace.".to_string(),
    );
    let err_hallucinated = EpistemicAuditorAgent
        .execute("audit", &ws, &bb_hallucinated)
        .unwrap_err()
        .to_string();
    assert!(err_hallucinated.contains("HALLUCINATION DETECTED & REJECTED"));
    assert!(bb_hallucinated.contains_key("systemic#hallucination_caught"));

    // 3. Empty promise without code or path evidence is rejected
    let bb_promise = MissionBlackboard::default();
    bb_promise.insert(
        "WorkerC".to_string(),
        "I have fixed the bug and completed the task.".to_string(),
    );
    let err_promise = EpistemicAuditorAgent
        .execute("audit", &ws, &bb_promise)
        .unwrap_err()
        .to_string();
    assert!(err_promise.contains("EMPTY PROMISE REJECTED"));
    assert!(bb_promise.contains_key("systemic#empty_promises_caught"));

    // 4. Malformed EvidenceRecord is rejected
    let bb_bad_ir = MissionBlackboard::default();
    bb_bad_ir.insert("EvidenceRecord::bad".to_string(), "{not json".to_string());
    let err_ir = EpistemicAuditorAgent
        .execute("audit", &ws, &bb_bad_ir)
        .unwrap_err()
        .to_string();
    assert!(err_ir.contains("UNFOUNDED LIES / UNBACKED ASSERTIONS REJECTED"));
    assert!(bb_bad_ir.contains_key("systemic#lies_caught"));

    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn consensus_mediator_signal_classification_and_execution() {
    let ws = Path::new(".");

    // Routine signals only -> consensus reached
    let bb_healthy = MissionBlackboard::default();
    bb_healthy.insert(
        "SafetyAgent".to_string(),
        "Safety protocols verified.".to_string(),
    );
    bb_healthy.insert(
        "SecurityAgent".to_string(),
        "Security audit passed.".to_string(),
    );
    bb_healthy.insert("EmptyAgent".to_string(), "   ".to_string());

    let (crit, healthy) = summarize_swarm_signals(&bb_healthy, "ConsensusMediatorAgent");
    assert!(crit.is_empty());
    assert_eq!(healthy.len(), 2);

    let ok_res = ConsensusMediatorAgent
        .execute("mediate", ws, &bb_healthy)
        .unwrap();
    assert!(ok_res.contains("Weighted consensus reached"));

    // Partial conflict (some critical, some healthy) -> Err with CONFLICT
    let bb_conflict = MissionBlackboard::default();
    bb_conflict.insert("SafetyAgent".to_string(), "All good".to_string());
    bb_conflict.insert(
        "GmcpAgent".to_string(),
        "Endpoint health status: DEGRADED".to_string(),
    );
    let conflict_err = ConsensusMediatorAgent
        .execute("mediate", ws, &bb_conflict)
        .unwrap_err()
        .to_string();
    assert!(conflict_err.contains("CONFLICT:"));
    assert!(conflict_err.contains("Consensus not reached"));

    // Swarm-wide distress (all critical, no healthy) -> Err with swarm-wide distress
    let bb_distress = MissionBlackboard::default();
    bb_distress.insert(
        "ResourceArbitratorAgent".to_string(),
        "OOM Critical Risk: true".to_string(),
    );
    bb_distress.insert(
        "WorkerAgent".to_string(),
        "Status test: FAILED | governance_block".to_string(),
    );
    let distress_err = ConsensusMediatorAgent
        .execute("mediate", ws, &bb_distress)
        .unwrap_err()
        .to_string();
    assert!(distress_err.contains("swarm-wide distress"));
    assert!(distress_err.contains("Consensus rejected"));
}

#[test]
fn extract_patch_request_json_handles_valid_and_invalid_inputs() {
    let valid_goal = r#"Please apply this fix: {"files": [{"path": "src/lib.rs", "content": "fn a() {}"}]} thanks"#;
    let extracted = extract_patch_request_json(valid_goal).unwrap();
    assert!(extracted.starts_with('{') && extracted.ends_with('}'));
    let parsed: serde_json::Value = serde_json::from_str(&extracted).unwrap();
    assert_eq!(parsed["files"].as_array().unwrap().len(), 1);

    // Missing "files" field
    assert!(extract_patch_request_json(r#"{"patches": []}"#).is_none());
    // "files" is not an array
    assert!(extract_patch_request_json(r#"{"files": "src/lib.rs"}"#).is_none());
    assert!(extract_patch_request_json(r#"{"files": {"path": "a.rs"}}"#).is_none());
    // Unbalanced or reversed braces / invalid JSON
    assert!(extract_patch_request_json(r#"{"files": [1, 2"#).is_none());
    assert!(extract_patch_request_json(r#"no braces at all"#).is_none());
    assert!(extract_patch_request_json(r#"} reversed {"#).is_none());
    assert!(extract_patch_request_json(r#"{not valid json}"#).is_none());
}

#[test]
fn is_remote_inference_endpoint_distinguishes_remote_from_loopback_and_invalid() {
    // Remote HTTP/HTTPS endpoints
    assert!(is_remote_inference_endpoint("https://api.openai.com/v1"));
    assert!(is_remote_inference_endpoint(
        "  HTTPS://api.anthropic.com/v1 "
    ));
    assert!(is_remote_inference_endpoint("http://10.0.0.42:8000/v1"));

    // Loopback addresses are local substrates, not remote egress
    assert!(!is_remote_inference_endpoint("http://localhost:11434/v1"));
    assert!(!is_remote_inference_endpoint("https://LOCALHOST:8000/v1"));
    assert!(!is_remote_inference_endpoint("http://127.0.0.1:8080/v1"));
    assert!(!is_remote_inference_endpoint("http://[::1]:8000/v1"));
    assert!(!is_remote_inference_endpoint("http://0.0.0.0:30000/v1"));

    // Missing or non-HTTP schemes
    assert!(!is_remote_inference_endpoint(""));
    assert!(!is_remote_inference_endpoint("api.openai.com/v1"));
    assert!(!is_remote_inference_endpoint("ftp://api.openai.com/v1"));
    assert!(!is_remote_inference_endpoint("ws://api.openai.com/v1"));
}

#[test]
fn admin_agent_gates_mutating_actions_behind_admin_pulse_and_allows_read_only() {
    // Read-only actions in ACTION_ORDER match without "admin pulse"
    assert_eq!(
        AdminAgent::match_action("check substrate status"),
        Some("status_health".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("check cluster health"),
        Some("status_health".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("show engine version"),
        Some("version".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("inspect identity"),
        Some("identity".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("list all models"),
        Some("list_models".to_string())
    );

    // Mutating actions must NOT trigger without "admin pulse" prefix
    for casual_goal in [
        "sync the workspace manifest",
        "audit repository compliance",
        "verify package alignment",
        "check the release notes",
        "deep-scan home directory",
        "install new dependencies",
        "initialize the workspace",
        "uninstall the package",
        "remove unused imports",
    ] {
        assert_eq!(
            AdminAgent::match_action(casual_goal),
            None,
            "mutating keyword in '{casual_goal}' must not match without 'admin pulse'"
        );
    }

    // Mutating actions DO trigger when prefixed with "admin pulse"
    assert_eq!(
        AdminAgent::match_action("admin pulse: sync"),
        Some("sync".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: audit"),
        Some("audit".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: verify"),
        Some("verify".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: release"),
        Some("release".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: deep-scan"),
        Some("deep_scan".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: install"),
        Some("install".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: initialize"),
        Some("install".to_string())
    );
    // Note: "uninstall" contains "install" as a substring and "install"
    // precedes "uninstall" in ACTION_ORDER, so "remove" is the unambiguous
    // trigger for "uninstall".
    assert_eq!(
        AdminAgent::match_action("admin pulse: uninstall"),
        Some("install".to_string())
    );
    assert_eq!(
        AdminAgent::match_action("admin pulse: remove"),
        Some("uninstall".to_string())
    );

    // ACTION_ORDER precedence: sync precedes release
    assert_eq!(
        AdminAgent::match_action("admin pulse: sync and release"),
        Some("sync".to_string())
    );
}

#[test]
fn fast_path_agent_executions_are_hermetic_on_reflex_and_status_queries() {
    let ws = Path::new(".");
    let bb = MissionBlackboard::default();

    // EvolutionAgent skips background audit on status/version/models/identity queries
    let evo = EvolutionAgent.execute("check version", ws, &bb).unwrap();
    assert!(evo.contains("drift audit not run for status queries"));

    // SelfHealingAgent skips health check on reflex queries
    let heal = SelfHealingAgent.execute("whoami", ws, &bb).unwrap();
    assert!(heal.contains("no health check runs for reflex queries"));

    // LibraryScoutAgent skips scouting on reflex queries
    let scout = LibraryScoutAgent.execute("ls -la", ws, &bb).unwrap();
    assert!(scout.contains("No library scouting applies to this query"));

    // AdminAgent executes version and fallback cleanly
    let ver = AdminAgent.execute("version", ws, &bb).unwrap();
    assert!(ver.contains("SUSI Engine Version: v"));
    let fallback = AdminAgent.execute("unmatched goal text", ws, &bb).unwrap();
    assert_eq!(fallback, "AdminAgent: Monitoring technical intent...");
}
