// SUSI Substrate Evolution Manager
// Test-Driven Evolution Substrate, implementing identity.json Mandate 20's
// Alpha-Self Evolution Order (Motion -> Architecture -> Structure -> Logic) —
// not the unrelated release-gate "Motion Rule" (Pillar IV item 3); this file
// predates the renaming that split those two concepts apart and originally
// called this one "Motion Rule Protocol" too, exactly the collision
// Mandate 20's own note warns about.

use crate::reflex_synth::ReflexSynthesizer;
use std::collections::HashMap;
use std::path::Path;
use susi_error::EaiResult;
use susi_sandbox::manager::SusiAuditLogger;

pub struct EvolutionManager;

impl EvolutionManager {
    /// Autonomous Drift Detection
    /// Periodic audit of the substrate health and capability surface.
    pub fn perform_autonomous_drift_audit(workspace: &Path) -> EaiResult<String> {
        // 1. Audit for High-Frequency Capability Gaps
        let gap = Self::detect_high_frequency_gap(workspace);

        // 2. Audit for Staged Experience (Substrate Ingestion Motion)
        let ingestion_res =
            crate::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(workspace)?;

        // Constraint-Free Evolution - Bottleneck Detection
        let bottlenecks = Self::detect_bottlenecks(workspace);

        // 3. If a high-frequency gap is detected and not yet synthesized, trigger synthesis
        if gap != "calculate square root" {
            // Heuristic check for non-default gap
            eprintln!("[Evolution Manager] Capability drift detected: {}. Initializing autonomous repair...", gap);
            let res = ReflexSynthesizer::distill_native_reflex(&gap, workspace)?;
            return Ok(format!(
                "Autonomous evolution successful: {}\n\n{}",
                res, bottlenecks
            ));
        }

        Ok(format!(
            "Substrate Optimal. {}\n\n{}",
            ingestion_res, bottlenecks
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

    pub fn detect_high_frequency_gap(workspace: &Path) -> String {
        let log_content = SusiAuditLogger::read_audit_log(workspace, 100);
        let mut intent_freq = HashMap::new();

        for line in log_content.lines() {
            if line.contains("[MISSION_START]") {
                if let Some(intent) = line.split("[MISSION_START]").nth(1) {
                    let trimmed = intent.trim();
                    if trimmed.len() > 3 {
                        *intent_freq.entry(trimmed.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }

        intent_freq
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(intent, _)| intent)
            .unwrap_or_else(|| "calculate square root".to_string())
    }
}
