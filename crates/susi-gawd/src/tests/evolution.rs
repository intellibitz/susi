//! Unit tests of `evolution.rs`, kept beside the crate's other suites.
use super::*;

#[test]
fn bottlenecks_count_real_failure_types_not_the_word_failed() {
    let ws = std::env::temp_dir().join(format!("susi_evo_bottleneck_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    let entry = |t: &str, d: &str| serde_json::json!({ "type": t, "details": d }).to_string();
    let lines = [
        entry("LATENCY_VIOLATION", "3ms"),
        entry("HALLUCINATION_DETECTED", "x"),
        entry("MISSION_START", "retry the failed build"),
    ];
    std::fs::write(ws.join(".susi/audit.log"), lines.join("\n")).unwrap();
    let report = EvolutionManager::detect_bottlenecks(&ws);
    assert!(
        report.contains("latency violations (>2ms)**: 1"),
        "{report}"
    );
    assert!(report.contains("**HALLUCINATION_DETECTED**: 1"), "{report}");
    assert!(!report.contains("MISSION_START"), "{report}");
    assert!(!report.contains("Mutation"), "{report}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn frequent_intents_come_from_json_audit_entries_with_a_floor() {
    let ws = std::env::temp_dir().join(format!("susi_evo_gap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    let entry = |t: &str, d: &str| serde_json::json!({ "type": t, "details": d }).to_string();
    let mut lines = vec![
        entry("WEB_MISSION_START", "summarize the logs"),
        entry("WEB_MISSION_START", "summarize the logs"),
        entry("LATENCY_VIOLATION", "summarize the logs"),
    ];
    std::fs::write(ws.join(".susi/audit.log"), lines.join("\n")).unwrap();
    assert_eq!(EvolutionManager::detect_high_frequency_gap(&ws), None);

    lines.push(entry("MISSION_START", "summarize the logs"));
    std::fs::write(ws.join(".susi/audit.log"), lines.join("\n")).unwrap();
    assert_eq!(
        EvolutionManager::detect_high_frequency_gap(&ws),
        Some(("summarize the logs".to_string(), 3))
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// The audit logger signs each entry with the host key under the
/// env-resolved substrate home; tests in this binary that swap `HOME`
/// and `SUSI_HOME` hold this lock, so a test that writes audit entries
/// holds it too or its key lookup lands in another test's deleted home.
fn audit_env() -> std::sync::MutexGuard<'static, ()> {
    crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn gate_workspace(intent: &str, traces: &[(&str, &str)]) -> std::path::PathBuf {
    let ws = std::env::temp_dir().join(format!(
        "susi_evo_gate_{}_{}",
        std::process::id(),
        intent.len()
    ));
    let _ = std::fs::remove_dir_all(&ws);
    let susi = ws.join(".susi");
    std::fs::create_dir_all(&susi).unwrap();
    // Three starts is the frequency floor — the gate decides from there.
    let entry = serde_json::json!({ "type": "MISSION_START", "details": intent }).to_string();
    std::fs::write(
        susi.join("audit.log"),
        [entry.clone(), entry.clone(), entry].join("\n"),
    )
    .unwrap();
    if !traces.is_empty() {
        let body: String = traces
            .iter()
            .map(|(goal, outcome)| {
                serde_json::json!({
                    "schema_version": 1,
                    "mission_id": "m",
                    "goal": goal,
                    "outcome": outcome,
                    "route": "swarm",
                    "tools": [],
                    "agents": [],
                    "evidence_entries": 0,
                    "duration_secs": null,
                    "timestamp": 1
                })
                .to_string()
                    + "\n"
            })
            .collect();
        std::fs::write(susi.join("mission_traces.jsonl"), body).unwrap();
    }
    ws
}

#[test]
fn failed_intent_is_vetoed_before_synthesis() {
    let _env = audit_env();
    let ws = gate_workspace(
        "vetoed mission intent",
        &[("vetoed mission intent", "FAILED")],
    );
    let report = EvolutionManager::evolve_recurring_intent(&ws).unwrap();
    assert!(report.contains("VETOED"), "{report}");
    // A vetoed intent never consumed an attempt slot.
    assert!(std::fs::read_dir(ws.join("reflexes")).is_err());
    let log = std::fs::read_to_string(ws.join(".susi/audit.log")).unwrap();
    assert!(log.contains("EVOLUTION_REFLEX_VETO"), "{log}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn observed_but_unproven_intent_is_deferred() {
    let _env = audit_env();
    let ws = gate_workspace(
        "deferred mission intent",
        &[("deferred mission intent", "COMPLETE")],
    );
    let report = EvolutionManager::evolve_recurring_intent(&ws).unwrap();
    assert!(report.contains("DEFERRED"), "{report}");
    let log = std::fs::read_to_string(ws.join(".susi/audit.log")).unwrap();
    assert!(log.contains("EVOLUTION_REFLEX_DEFER"), "{log}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn detect_bottlenecks_and_drift_audit_report_clean_state_on_empty_workspace() {
    let ws = std::env::temp_dir().join(format!("susi_evo_empty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    let bottlenecks = EvolutionManager::detect_bottlenecks(&ws);
    assert!(bottlenecks.contains("No substrate bottlenecks detected"));
    assert_eq!(EvolutionManager::detect_high_frequency_gap(&ws), None);
    assert_eq!(EvolutionManager::evolve_recurring_intent(&ws), None);
    let audit = EvolutionManager::perform_autonomous_drift_audit(&ws).unwrap();
    assert!(audit.contains("No intent recurred often enough"));
    let _ = std::fs::remove_dir_all(&ws);
}
