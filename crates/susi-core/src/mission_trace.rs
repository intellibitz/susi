//! Mission-level outcome trace — the learning loop's unit of experience.
//!
//! Tool receipts (`capture.rs`) record what each dispatch did; this records
//! what the *mission* achieved: intent, the route that handled it, the tools
//! and agents it consumed, its evidence count, wall-clock cost, and verdict.
//! Every mission that persists an inspectable trace also emits one of these,
//! appended JSONL to `.susi/mission_traces.jsonl` and linked into the context
//! graph as a mission outcome observation, so later stages (distillation
//! staging, reflex promotion, history retrieval) read one schema'd record
//! instead of scraping prose.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use susi_error::{eai_err as anyhow, EaiResult};

use crate::context_graph::ContextGraph;

/// Bumped when fields are added or reinterpreted; readers must tolerate
/// unknown fields so newer traces never break older trainers.
pub const SCHEMA_VERSION: u32 = 1;

/// The trace is a learning record, not a transcript — goals bound at this
/// length so a pasted document cannot dominate the file.
const MAX_GOAL_CHARS: usize = 240;
const MAX_FIELD_CHARS: usize = 120;
const MAX_LISTED: usize = 64;

/// One mission's outcome record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionTrace {
    pub schema_version: u32,
    /// Evidence-session id when one was active, else `"mission-<ts>"`.
    pub mission_id: String,
    /// Bounded, credential-redacted goal text.
    pub goal: String,
    /// Terminal report status (`COMPLETE`, `FAILED`, `BLOCKED`, ...).
    pub outcome: String,
    /// Which path produced the answer — the label Tier-0 distillation learns
    /// to predict (`swarm`, `fast-path`, `governance-block`, ...).
    pub route: String,
    /// Distinct tool/action names observed in the mission's interactions.
    pub tools: Vec<String>,
    /// Recruited agent names.
    pub agents: Vec<String>,
    /// Count of interaction/evidence entries the report carried.
    pub evidence_entries: usize,
    /// Wall-clock mission time when an evidence session bounds it.
    pub duration_secs: Option<u64>,
    pub timestamp: u64,
}

impl MissionTrace {
    pub fn new(mission_id: impl Into<String>, goal: &str, outcome: &str, route: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            mission_id: mission_id.into(),
            goal: susi_config::redact_credentials(&bound_chars(goal, MAX_GOAL_CHARS)),
            outcome: bound_chars(outcome, MAX_FIELD_CHARS),
            route: bound_chars(route, MAX_FIELD_CHARS),
            tools: Vec::new(),
            agents: Vec::new(),
            evidence_entries: 0,
            duration_secs: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// True when the mission reached a verified-success verdict — the flag
    /// promotion rules key on, so it follows `status`, never prose.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome.as_str(), "SUCCESS" | "COMPLETE")
    }

    /// Emit the record to its sinks. Best-effort at each sink — a full disk
    /// or an unwired graph must not fail a mission that already finished —
    /// but errors surface as `Err` so callers can observe a total failure.
    pub fn emit(&self, workspace: &Path) -> EaiResult<()> {
        // 1. Context graph: outcome observation linked to the mission node.
        ContextGraph::global().record_agent_observation(
            Some(&self.mission_id),
            "MissionTrace",
            &format!(
                "outcome={} route={} tools={} agents={} evidence={} duration_secs={}",
                self.outcome,
                self.route,
                self.tools.join(","),
                self.agents.join(","),
                self.evidence_entries,
                self.duration_secs
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ),
            workspace,
        );

        // 2. Append-only JSONL beside the workspace's other mission state.
        let susi_dir = workspace.join(".susi");
        std::fs::create_dir_all(&susi_dir)
            .map_err(|e| anyhow!("mission trace dir create failed: {e}"))?;
        let mut line = serde_json::to_vec(self)
            .map_err(|e| anyhow!("mission trace serialization failed: {e}"))?;
        line.push(b'\n');
        let _lock = crate::commit_log::FileLock::acquire(&susi_dir, "mission_traces")
            .ok_or_else(|| anyhow!("mission trace lock acquisition failed"))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(susi_dir.join("mission_traces.jsonl"))
            .map_err(|e| anyhow!("mission trace file open failed: {e}"))?;
        file.write_all(&line)
            .map_err(|e| anyhow!("mission trace write failed: {e}"))
    }
}

fn bound_chars(input: &str, max: usize) -> String {
    input.chars().take(max).collect()
}

/// Read every mission trace recorded under `workspace/.susi/`, skipping
/// lines that do not parse (older schema generations stay readable).
pub fn read_all(workspace: &Path) -> Vec<MissionTrace> {
    let path = workspace.join(".susi").join("mission_traces.jsonl");
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub fn bounded_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    items.into_iter().take(MAX_LISTED).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn emit_appends_redacted_parseable_records() {
        let ws = workspace();
        let mut trace = MissionTrace::new(
            "m-1",
            "deploy the api with token ghp_secret9",
            "COMPLETE",
            "swarm",
        );
        trace.tools = bounded_list(["exec_command".into(), "exec_command".into()]);
        trace.agents = bounded_list(["Coder".into()]);
        trace.evidence_entries = 3;
        trace.emit(ws.path()).unwrap();

        let body = std::fs::read_to_string(ws.path().join(".susi/mission_traces.jsonl")).unwrap();
        assert!(!body.contains("ghp_secret9"), "{body}");
        let parsed: Vec<MissionTrace> = read_all(ws.path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].route, "swarm");
        assert!(parsed[0].succeeded());
        assert_eq!(parsed[0].schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn emit_accumulates_and_tolerates_partial_lines() {
        let ws = workspace();
        for (id, outcome) in [("a", "COMPLETE"), ("b", "FAILED"), ("c", "BLOCKED")] {
            MissionTrace::new(id, "goal", outcome, "fast-path")
                .emit(ws.path())
                .unwrap();
        }
        let path = ws.path().join(".susi/mission_traces.jsonl");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("{not json\n");
        std::fs::write(&path, body).unwrap();
        let parsed = read_all(ws.path());
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed.iter().filter(|t| t.succeeded()).count(), 1);
    }

    #[test]
    fn long_goals_are_bounded() {
        let ws = workspace();
        let long = "x".repeat(MAX_GOAL_CHARS * 4);
        MissionTrace::new("m", &long, "COMPLETE", "swarm")
            .emit(ws.path())
            .unwrap();
        let parsed = read_all(ws.path());
        assert_eq!(parsed[0].goal.chars().count(), MAX_GOAL_CHARS);
    }

    #[test]
    fn success_verdict_follows_status_only() {
        for (outcome, ok) in [
            ("COMPLETE", true),
            ("SUCCESS", true),
            ("FAILED", false),
            ("BLOCKED", false),
            ("ABORTED", false),
            ("mission complete", false),
        ] {
            let t = MissionTrace::new("m", "g", outcome, "r");
            assert_eq!(t.succeeded(), ok, "{outcome}");
        }
    }
}
