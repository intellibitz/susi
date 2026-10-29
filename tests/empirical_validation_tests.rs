#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! SUSI Empirical Validation Test Suite
//! Implements automated checks based on the architectural test plan

#![allow(missing_docs)] // integration test crate: no public API to document

use std::path::Path;
use std::sync::OnceLock;
use susi_gawd::agents::GawdAgent;
use susi_paths::test_env::EnvGuard;

fn test_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("susi_empirical_{}", std::process::id()))
}

/// Missions run in a scratch workspace, never the checkout: under the
/// isolated HOME a recovery fallback once launched aider in the real repo,
/// which wrote placeholder `Your Name` git identity into its `.git/config`.
fn scratch_workspace() -> std::path::PathBuf {
    let ws = test_home().join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

fn wire_test_substrate() {
    // The guard lives in a static, so it is never dropped: the isolated
    // environment holds for the whole test process, like the wiring.
    static SUBSTRATE_ENV: OnceLock<EnvGuard> = OnceLock::new();
    SUBSTRATE_ENV.get_or_init(|| {
        // Isolate HOME before any substrate wiring: the vendored MacPolicy /
        // EvidenceSession / cloud.env readers all resolve real ~/.susi state
        // otherwise, leaking host credentials and receipts into tests.
        let tmp = test_home();
        let _ = std::fs::create_dir_all(&tmp);
        // `isolated` drops the instance knobs, which outrank HOME/XDG: a
        // dev-instance launch must not redirect this process-lifetime
        // substrate (or its ports). Setting HOME through the guard also
        // moves the Windows profile (USERPROFILE) and its APPDATA and
        // LOCALAPPDATA, where the platform layout lives there (XDG_* is
        // ignored on Windows): setting HOME/USERPROFILE alone still read
        // cloud.env and the MAC/evidence state from the operator's real
        // AppData.
        let mut env = EnvGuard::isolated();
        env.set("HOME", &tmp)
            .set("XDG_CONFIG_HOME", tmp.join("xdg"))
            .set("XDG_DATA_HOME", tmp.join("xdg-data"))
            .set("XDG_CACHE_HOME", tmp.join("xdg-cache"))
            // Set the inference seam before any global registry or engine is
            // initialized.  The tests run in parallel, so setting it only in
            // the two inference tests leaves a race where another test can
            // bootstrap real cloud/MCP providers first.
            .set("SUSI_TEST_MOCK_INFERENCE", "true");
        // Feature planes talk only via plane_bus; integration tests must
        // register handlers the same way CLI/daemon composition does.
        susi_daemon::composition::wire_plane_bus();
        susi_tools::hooks::init(Box::new(susi_daemon::SusiEngineHooks));
        let _ = susi_tools::ToolRegistry::global();
        env
    });
}

/// Every substrate root resolves under the test home on every OS. On
/// Windows that needs the swapped profile's AppData: `xdg.rs` reads
/// APPDATA/LOCALAPPDATA there, not `XDG_*`, and there is no legacy `~/.susi`
/// under the test home.
#[test]
fn substrate_dirs_resolve_under_the_test_home() {
    wire_test_substrate();
    let home = test_home();
    for dir in [
        susi_paths::SusiDirs::home_dir(),
        susi_paths::SusiDirs::config_dir(),
        susi_paths::SusiDirs::data_dir(),
        susi_paths::SusiDirs::cache_dir(),
    ] {
        assert!(
            dir.starts_with(&home),
            "{} is outside the test home {}",
            dir.display(),
            home.display()
        );
    }
}

#[test]
fn test_empirical_reflex_classification() {
    wire_test_substrate();
    let ws = scratch_workspace();
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
    // A debug build on a loaded Windows host can take twice the budget the
    // unix gate enforces; the reflex contract stays at 10 s everywhere else.
    #[cfg(windows)]
    const REFLEX_BUDGET_MS: u128 = 30_000;
    #[cfg(not(windows))]
    const REFLEX_BUDGET_MS: u128 = 10_000;
    assert!(duration.as_millis() < REFLEX_BUDGET_MS);
}

#[test]
fn test_empirical_epistemic_integrity_fictitious() {
    wire_test_substrate();
    let ws = scratch_workspace();
    let ama = susi_gawd::ama::SusiMasterAgent::new();
    let res = ama.solve_clean_generative(
        "inspect the status of cargo module non_existent_quantum_crank",
        &ws,
        susi::SUSI_VERSION,
        None,
    );
    assert!(!res.contains("quantum_crank version 1.0.0 successfully deployed"));
    // Mock inference must not fall through to external-agent delegation.
    assert!(
        !ws.join(".susi").join("execution-agents").exists(),
        "mock-inference mission launched an external agent"
    );
}

// Both detectors load their governance patterns from
// `SusiDirs::config_dir()`, so they isolate first like every other test
// here: run before the first wiring, they read the operator's real config.
#[test]
fn test_empirical_safety_destructive_payload() {
    wire_test_substrate();
    let ws = Path::new(".");
    let res = susi_gawd::safety::SafetyDetector::audit_action("exec_command", "rm -rf /", ws);
    assert!(res.is_err());
}

#[test]
fn test_empirical_credential_masking() {
    wire_test_substrate();
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
    wire_test_substrate();
    let ws = std::env::current_dir().unwrap();
    let blackboard: susi_gawd::agents::MissionBlackboard =
        std::sync::Arc::new(susi_gawd::agents::HighDensityContextStore::new(10));
    let agent = susi_gawd::agents::GmcpAgent;
    let res = agent.execute("verify gmcp endpoints", &ws, &blackboard);
    assert!(res.is_ok());
    let output = res.unwrap();
    assert!(
        output.contains("GmcpAgent") && output.contains("OPTIMAL"),
        "unexpected GmcpAgent output: {output}"
    );
}
