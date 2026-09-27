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
                "Most frequent recent intent: '{intent}' ({count}x) — a candidate for a WASI reflex (`ReflexSynthesizer::synthesize_wasm_reflex`)."
            ),
            None => "No intent recurred often enough to suggest a reflex.".to_string(),
        };
        let ingestion =
            crate::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(workspace)?;
        let bottlenecks = Self::detect_bottlenecks(workspace);
        Ok(format!(
            "Drift audit: {frequent}\n{ingestion}\n\n{bottlenecks}"
        ))
    }

    /// Bottleneck Detection
    /// Analyzes audit logs for high latency and mission failures.
    pub fn detect_bottlenecks(workspace: &Path) -> String {
        let log_content = SusiAuditLogger::read_audit_log(workspace, 500);
        let mut latency_violations = 0;
        let mut mission_failures = 0;

        for line in log_content.lines() {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                let event_type = entry["type"].as_str().unwrap_or_default();
                let details = entry["details"].as_str().unwrap_or_default();

                if event_type == "LATENCY_VIOLATION" {
                    latency_violations += 1;
                } else if event_type == "MISSION_FAILED"
                    || event_type == "TOOL_FAILURE"
                    || details.contains("failed")
                {
                    mission_failures += 1;
                }
            }
        }

        if latency_violations == 0 && mission_failures == 0 {
            return "No substrate bottlenecks detected. Performance nominal.".to_string();
        }

        let mut report = format!(
            "# Substrate Bottleneck Analysis\n\n\
            - **Latency Violations (>2ms)**: {}\n\
            - **Mission Failures**: {}\n\n\
            ### Recommended Genome Mutations:\n",
            latency_violations, mission_failures
        );

        if latency_violations > 0 {
            report.push_str("- **Mutation**: Synthesize GPU-accelerated reflex for pattern recognition to saturate hardware.\n");
        }
        if mission_failures > 0 {
            report.push_str("- **Mutation**: Trigger specialist agent synthesis for high-frequency failure signatures.\n");
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
}
