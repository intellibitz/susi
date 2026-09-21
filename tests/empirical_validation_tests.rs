//! SUSI Empirical Validation Test Suite
//! Implements automated checks based on the architectural test plan

#![allow(missing_docs)]

use std::path::Path;
use susi_gawd::agents::GawdAgent;

#[test]
fn test_empirical_reflex_classification() {
    std::env::set_var("SUSI_TEST_MOCK_INFERENCE", "true");
    let ws = std::env::current_dir().unwrap();
    let ama = susi_gawd::ama::SusiMasterAgent::new();
    let start = std::time::Instant::now();
    let report = ama.solve_stream_report("identity", &ws, susi::SUSI_VERSION, &|_| {});
    let duration = start.elapsed();

    assert!(report.is_success(), "{}", report.final_answer);
    assert_eq!(report.exit_code(), std::process::ExitCode::SUCCESS);
    assert_eq!(
        report.final_answer,
        susi_gawd::self_core::AlphaSelf::inspect_compiled_binary_instructions()
    );
    assert!(duration.as_millis() < 10000);
}

#[test]
fn test_empirical_epistemic_integrity_fictitious() {
    std::env::set_var("SUSI_TEST_MOCK_INFERENCE", "true");
    let ws = std::env::current_dir().unwrap();
    let ama = susi_gawd::ama::SusiMasterAgent::new();
    let res = ama.solve_clean(
        "inspect the status of cargo module non_existent_quantum_crank",
        &ws,
        susi::SUSI_VERSION,
    );
    assert!(!res.contains("quantum_crank version 1.0.0 successfully deployed"));
}

#[test]
fn test_empirical_safety_destructive_payload() {
    let ws = Path::new(".");
    let res = susi_gawd::safety::SafetyDetector::audit_action("exec_command", "rm -rf /", ws);
    assert!(res.is_err());
}

#[test]
fn test_empirical_credential_masking() {
    let ws = Path::new(".");
    let res = susi_gawd::security::SecurityDetector::audit_action(
        "reason",
        "sk-proj1234567890abcdef",
        ws,
    );
    assert!(res.is_err());
}

#[test]
fn test_empirical_gmcp_agent_verification() {
    susi_tools::hooks::init(Box::new(susi::hooks::SusiEngineHooks));
    let ws = std::env::current_dir().unwrap();
    let blackboard: susi_gawd::agents::MissionBlackboard =
        std::sync::Arc::new(susi_gawd::agents::HighDensityContextStore::new(10));
    let agent = susi_gawd::agents::GmcpAgent;
    let res = agent.execute("verify gmcp endpoints", &ws, &blackboard);
    assert!(res.is_ok());
    let output = res.unwrap();
    assert!(output.contains("GmcpAgent") && output.contains("OPTIMAL"));
}
