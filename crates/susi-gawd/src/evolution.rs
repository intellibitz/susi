// SUSI Substrate Evolution Manager
// Test-Driven Evolution Substrate, implementing identity.json Mandate 20's
// Alpha-Self Evolution Order (Motion -> Architecture -> Structure -> Logic) —
// not the unrelated release-gate "Motion Rule" (Pillar IV item 3); this file
// predates the renaming that split those two concepts apart and originally
// called this one "Motion Rule Protocol" too, exactly the collision
// Mandate 20's own note warns about.

use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::SusiAuditLogger;
use std::collections::HashMap;
use std::path::Path;

pub struct EvolutionManager;

impl EvolutionManager {
    /// Autonomous Drift Detection
    /// Periodic audit of the substrate health and capability surface. It
    /// reports observations; it does not write code into the workspace.
    pub fn perform_autonomous_drift_audit(workspace: &Path) -> EaiResult<String> {
        let frequent = match Self::detect_high_frequency_gap(workspace) {
            Some((intent, count)) => format!(
                "Most frequent recent intent: '{intent}' ({count}x) — a candidate for a WASI reflex (`EvolutionManager::evolve_recurring_intent`)."
            ),
            None => "No intent recurred often enough to suggest a reflex.".to_string(),
        };
        let ingestion =
            crate::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(workspace)?;
        let staging =
            crate::susi_core::receipt_archive::ReceiptArchive::staging_health_summary(workspace);
        let staging_line = staging.summary();
        let bottlenecks = Self::detect_bottlenecks(workspace);
        Ok(format!(
            "Drift audit: {frequent}\n{ingestion}\n{staging_line}\n\n{bottlenecks}"
        ))
    }

    /// Act on the drift signal -- the self-improvement step of the loop: when
    /// a mission intent recurred (see [`detect_high_frequency_gap`]) and no
    /// reflex exists for it, synthesize one (`synthesize_capability_for`:
    /// model-written, published only after it compiles and runs in the WASI
    /// sandbox; else the probe). At most one attempt per intent per 24h
    /// (`reflexes/<slug>.attempted`), since each is a model call and a
    /// compile. Every outcome is audit-logged with the honest gap report.
    /// Returns that report, or `None` when there was nothing to do.
    ///
    /// [`detect_high_frequency_gap`]: Self::detect_high_frequency_gap
    pub fn evolve_recurring_intent(workspace: &Path) -> Option<String> {
        use crate::reflex_synth::{reflex_path, slug_for, ReflexSynthesizer};
        const RETRY_AFTER_SECS: u64 = 24 * 3600;
        let (intent, count) = Self::detect_high_frequency_gap(workspace)?;
        let slug = slug_for(&intent)?;
        let wasm = reflex_path(&slug);
        if wasm.exists() {
            return None;
        }

        // Promotion is earned by verified outcomes, not frequency alone:
        // the mission-trace record must show enough successes with no
        // unresolved failure in the recent window. A deferred or vetoed
        // intent does not consume the 24h attempt budget — it was never
        // attempted.
        let traces = crate::susi_core::mission_trace::read_all(workspace);
        match crate::susi_core::mission_trace::promotion_status(&traces, &intent) {
            crate::susi_core::mission_trace::PromotionStatus::Promotable { successes } => {
                eprintln!(
                    "[Evolution] Intent '{intent}' promoted with {successes} verified successes"
                );
            }
            crate::susi_core::mission_trace::PromotionStatus::Vetoed { reason } => {
                let report = format!("reflex promotion VETOED for '{intent}': {reason}");
                SusiAuditLogger::log_event(workspace, "EVOLUTION_REFLEX_VETO", &report);
                return Some(report);
            }
            crate::susi_core::mission_trace::PromotionStatus::Insufficient {
                successes,
                failures,
            } => {
                // No trace history for this intent at all means the trace
                // stream predates it — frequency evidence stands alone
                // (legacy behavior). Traces that exist but show too little
                // success defer promotion until evidence accumulates.
                if !crate::susi_core::mission_trace::has_traces_for(&traces, &intent) {
                    // fall through to legacy promotion
                } else {
                    let report = format!(
                        "reflex promotion DEFERRED for '{intent}': {successes} verified successes, \
                         {failures} failures — needs {} successes with a clean window",
                        crate::susi_core::mission_trace::MIN_PROMOTION_SUCCESSES
                    );
                    SusiAuditLogger::log_event(workspace, "EVOLUTION_REFLEX_DEFER", &report);
                    return Some(report);
                }
            }
        }
        let marker = wasm.with_extension("attempted");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if now.saturating_sub(last) < RETRY_AFTER_SECS {
            return None;
        }
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, now.to_string());
        let outcome = ReflexSynthesizer::synthesize_capability_for(&slug, &intent, workspace);
        let report = format!(
            "recurring intent '{intent}' ({count}x): {}",
            ReflexSynthesizer::gap_report(&slug, &outcome)
        );
        SusiAuditLogger::log_event(workspace, "EVOLUTION_REFLEX_ATTEMPT", &report);
        Some(report)
    }

    /// Bottleneck Detection: latency violations and failure-signal events
    /// among the last 500 audit entries, counted by their real event type
    /// (`*_FAILED`, `*_VIOLATION`, `*_DETECTED`). It used to count
    /// `MISSION_FAILED` / `TOOL_FAILURE` — types nothing logs — plus any
    /// entry whose details merely contained "failed", and followed with
    /// canned "genome mutation" advice unrelated to the data.
    pub fn detect_bottlenecks(workspace: &Path) -> String {
        let log_content = SusiAuditLogger::read_audit_log(workspace, 500);
        let mut latency_violations = 0usize;
        let mut failures: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for line in log_content.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let event_type = entry["type"].as_str().unwrap_or_default();
            if event_type == "LATENCY_VIOLATION" {
                latency_violations += 1;
            } else if event_type.ends_with("_FAILED")
                || event_type.ends_with("_VIOLATION")
                || event_type.ends_with("_DETECTED")
            {
                *failures.entry(event_type.to_string()).or_default() += 1;
            }
        }

        if latency_violations == 0 && failures.is_empty() {
            return "No substrate bottlenecks detected in the last 500 audit entries.".to_string();
        }
        let mut report = format!(
            "# Substrate Bottleneck Analysis (last 500 audit entries)\n\n\
            - **Reflex latency violations (>2ms)**: {latency_violations}\n"
        );
        for (event_type, count) in &failures {
            report.push_str(&format!("- **{event_type}**: {count}\n"));
        }
        report
    }

    /// The most frequent mission intent among the last 100 audit entries
    /// (`*MISSION_START` records), if it recurred at least three times.
    pub fn detect_high_frequency_gap(workspace: &Path) -> Option<(String, usize)> {
        const MIN_OCCURRENCES: usize = 3;
        let log_content = SusiAuditLogger::read_audit_log(workspace, 100);
        let mut intent_freq: HashMap<String, usize> = HashMap::new();
        for line in log_content.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if !entry["type"]
                .as_str()
                .is_some_and(|t| t.ends_with("MISSION_START"))
            {
                continue;
            }
            let intent = entry["details"].as_str().unwrap_or_default().trim();
            if intent.len() > 3 {
                *intent_freq.entry(intent.to_string()).or_insert(0) += 1;
            }
        }
        intent_freq
            .into_iter()
            .filter(|(_, count)| *count >= MIN_OCCURRENCES)
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
    }
}

#[cfg(test)]
mod tests {
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
}
