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

/// Whitespace-token overlap between a query goal and a recorded trace.
fn token_overlap(goal: &str, trace_goal: &str) -> f32 {
    // Stopwords carry no intent signal — "the" alone must never make two
    // missions look similar.
    const STOPWORDS: &[&str] = &["the", "and", "for", "with", "this", "that", "from"];
    let tokens = |s: &str| {
        s.split_whitespace()
            .map(|t| {
                t.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string()
            })
            .filter(|t| t.len() > 2 && !STOPWORDS.contains(&t.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let (a, b) = (tokens(goal), tokens(trace_goal));
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f32 / union as f32
}

/// Similarity floor for a trace to count as "the same kind of mission".
const SIMILARITY_FLOOR: f32 = 0.15;

/// Retrieve the `limit` traces most similar to `goal` — the retrieval stage
/// of the loop: prior outcomes for the same kind of intent.
pub fn similar<'a>(goal: &str, traces: &'a [MissionTrace], limit: usize) -> Vec<&'a MissionTrace> {
    let mut scored: Vec<(f32, &'a MissionTrace)> = traces
        .iter()
        .map(|t| (token_overlap(goal, &t.goal), t))
        .filter(|(s, _)| *s >= SIMILARITY_FLOOR)
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.timestamp.cmp(&a.1.timestamp))
    });
    scored.into_iter().take(limit).map(|(_, t)| t).collect()
}

/// An explainable difficulty estimate — every factor is named so the routed
/// decision is auditable, not a hidden weight.
#[derive(Debug, Clone, PartialEq)]
pub struct Difficulty {
    /// 0.0..=1.0 composite.
    pub score: f32,
    /// No similar mission has ever run.
    pub novel: bool,
    /// Fraction of similar missions that did not succeed.
    pub failure_rate: f32,
    /// Manifold risk contribution already applied.
    pub risk: crate::manifold::RiskProfile,
}

impl Difficulty {
    /// Hard enough that deliberation should widen (more candidates, deeper
    /// budgets) and routing should prefer the stronger tier.
    pub fn demands_deliberation(&self) -> bool {
        self.score >= 0.45
    }
}

/// Estimate difficulty for `goal` from retrieval history and manifold risk.
/// Novel intents and intents whose predecessors often failed route harder;
/// familiar, reliably-solved intents stay cheap.
pub fn difficulty(
    goal: &str,
    traces: &[MissionTrace],
    risk: crate::manifold::RiskProfile,
) -> Difficulty {
    let neighbors = similar(goal, traces, 8);
    let novel = neighbors.is_empty();
    let failure_rate = if neighbors.is_empty() {
        0.0
    } else {
        neighbors.iter().filter(|t| !t.succeeded()).count() as f32 / neighbors.len() as f32
    };
    let risk_weight = match risk {
        crate::manifold::RiskProfile::Low => 0.0,
        crate::manifold::RiskProfile::Medium => 0.15,
        crate::manifold::RiskProfile::High => 0.25,
        crate::manifold::RiskProfile::Critical => 0.4,
    };
    let novelty_weight = if novel { 0.35 } else { 0.0 };
    let score = (novelty_weight + 0.4 * failure_rate + risk_weight).clamp(0.0, 1.0);
    Difficulty {
        score,
        novel,
        failure_rate,
        risk,
    }
}

/// One-line history brief for prompt injection: what similar missions did
/// and how they ended. Empty when nothing similar exists.
pub fn history_brief(goal: &str, traces: &[MissionTrace], limit: usize) -> String {
    let lines: Vec<String> = similar(goal, traces, limit)
        .iter()
        .map(|t| {
            format!(
                "- \"{}\" -> {} via {} (tools: {})",
                t.goal,
                t.outcome,
                t.route,
                t.tools.join(",")
            )
        })
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "Prior outcomes for similar goals:\n{}\n\n",
            lines.join("\n")
        )
    }
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
    fn retrieval_ranks_similar_goals_and_difficulty_reads_history() {
        let ws = workspace();
        // Two deploys failed, one passed; an unrelated trace is noise.
        for (goal, outcome) in [
            ("deploy the api service", "FAILED"),
            ("deploy the api service again", "FAILED"),
            ("deploy the api service cleanly", "COMPLETE"),
            ("list directory contents", "COMPLETE"),
        ] {
            MissionTrace::new("m", goal, outcome, "swarm")
                .emit(ws.path())
                .unwrap();
        }
        let traces = read_all(ws.path());

        let hits = similar("deploy api service", &traces, 5);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|t| t.goal.contains("deploy")));

        let d = difficulty(
            "deploy api service",
            &traces,
            crate::manifold::RiskProfile::Low,
        );
        assert!(!d.novel);
        assert!((d.failure_rate - 2.0 / 3.0).abs() < 0.01);
        assert!(d.score > 0.2);

        // A novel intent reads as novel and difficult enough to widen search.
        let novel = difficulty(
            "refactor the compiler",
            &traces,
            crate::manifold::RiskProfile::High,
        );
        assert!(novel.novel);
        assert!(novel.demands_deliberation());

        let brief = history_brief("deploy api service", &traces, 2);
        assert!(brief.contains("Prior outcomes"));
        assert!(brief.contains("FAILED"));
        assert!(history_brief("unrelated xyz", &traces, 5).is_empty());
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
