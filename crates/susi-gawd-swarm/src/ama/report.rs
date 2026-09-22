//! Mission report + inspectable trace persistence.

use crate::amas::A2AMessage;
use serde::{Deserialize, Serialize};
use std::path::Path;
use susi_gawd_agents::agents::GawdAgentInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SusiMissionReport {
    pub goal: String,
    pub status: String,
    pub agents: Vec<GawdAgentInfo>,
    pub interactions: Vec<A2AMessage>,
    pub final_answer: String,
}

pub type SusiSwarmReport = SusiMissionReport;

impl SusiMissionReport {
    /// Only an explicitly successful mission may produce a success exit code.
    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "SUCCESS" | "COMPLETE")
    }

    /// CLI outcome derived from the report, never from generated prose.
    pub fn exit_code(&self) -> std::process::ExitCode {
        if self.is_success() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        }
    }

    /// Human-readable completion marker using the same outcome as the protocol.
    pub fn completion_message(&self) -> String {
        if self.is_success() {
            format!("[MISSION COMPLETE] Status: {}", self.status)
        } else {
            format!("[MISSION FAILED] Status: {}", self.status)
        }
    }

    pub fn to_protocol_format(&self, _is_ide_environment: bool) -> String {
        let mut full_thinking_trace = String::new();
        full_thinking_trace.push_str(&format!(
            "SUSI Mission Goal: {}\nStatus: {}\nAgents Recruited: {}\n\n",
            self.goal,
            self.status,
            self.agents.len()
        ));

        for agent in &self.agents {
            full_thinking_trace
                .push_str(&format!("- [Agent] {} ({})\n", agent.name, agent.provider));
        }

        for msg in &self.interactions {
            full_thinking_trace.push_str(&format!(
                "- [{}] Action: {} | Payload: {}\n",
                msg.sender, msg.action, msg.payload
            ));
        }

        let primary_step = serde_json::json!({
            "action": "supervise_mission_swarm",
            "action_input": { "goal": self.goal },
            "observation": format!("Mission status: {}", self.status),
            "thought": full_thinking_trace.trim()
        });

        let json_str = serde_json::to_string_pretty(&primary_step).unwrap_or_default();
        let trimmed_answer = self.final_answer.trim();

        if trimmed_answer.is_empty()
            || trimmed_answer.starts_with("[FAST-PATH COMPLETE]")
            || trimmed_answer == self.goal
        {
            json_str
        } else {
            format!("{}\n\n{}", json_str, trimmed_answer)
        }
    }

    /// Design principle Traceable reasoning: persist inspectable thought + tool
    /// provenance under the workspace `.susi/` tree (no secret bodies).
    pub fn persist_inspectable_trace(&self, workspace: &Path) {
        let dir = workspace.join(".susi");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("last_mission_trace.json");
        let protocol_raw = self.to_protocol_format(false);
        // Protocol may append the final answer after a blank line; parse JSON only.
        let json_part = protocol_raw
            .split("\n\n")
            .next()
            .unwrap_or(protocol_raw.as_str());
        let protocol: serde_json::Value =
            serde_json::from_str(json_part).unwrap_or_else(|_| serde_json::json!({}));
        let body = serde_json::json!({
            "goal": self.goal,
            "status": self.status,
            "agents": self.agents,
            "interactions": self.interactions,
            "final_answer": self.final_answer,
            "protocol": protocol,
            "protocol_raw": protocol_raw,
            "blackboard_path": ".susi/last_blackboard.json",
            "blackboard": load_persisted_blackboard(workspace),
        });
        let _ = std::fs::write(
            path,
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()),
        );
    }
}

fn load_persisted_blackboard(workspace: &Path) -> serde_json::Value {
    let path = workspace.join(".susi").join("last_blackboard.json");
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(serde_json::json!({})),
        Err(_) => serde_json::json!({ "entries": [], "agent_count": 0 }),
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn failure_evidence_controls_banner_and_exit_even_with_optimistic_prose() {
        for status in ["FAILED", "BLOCKED", "ABORTED", "UNVERIFIED", ""] {
            let report = SusiMissionReport {
                goal: "weather".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: "Mission complete. Everything worked.".into(),
            };
            assert!(!report.is_success());
            assert_eq!(report.exit_code(), std::process::ExitCode::FAILURE);
            assert!(report.completion_message().starts_with("[MISSION FAILED]"));
            assert!(!report.completion_message().contains("MISSION COMPLETE"));
            // The protocol emits its JSON envelope followed by the answer text.
            let rendered = report.to_protocol_format(false);
            let protocol = serde_json::Deserializer::from_str(&rendered)
                .into_iter::<serde_json::Value>()
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(protocol["observation"], format!("Mission status: {status}"));
        }
    }

    #[test]
    fn explicit_success_reports_have_success_banner_and_exit() {
        for status in ["SUCCESS", "COMPLETE"] {
            let report = SusiMissionReport {
                goal: "inspect".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: "Observed the requested file.".into(),
            };
            assert!(report.is_success());
            assert_eq!(report.exit_code(), std::process::ExitCode::SUCCESS);
            assert!(report
                .completion_message()
                .starts_with("[MISSION COMPLETE]"));
        }
    }

    #[test]
    fn protocol_preserves_failed_and_blocked_outcomes() {
        for status in ["FAILED", "BLOCKED", "ABORTED", "COMPLETE"] {
            let report = SusiMissionReport {
                goal: "test".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: String::new(),
            };
            let value: serde_json::Value =
                serde_json::from_str(&report.to_protocol_format(false)).unwrap();
            assert_eq!(value["observation"], format!("Mission status: {status}"));
        }
    }
}
