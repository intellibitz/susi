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
/// v2: `tools` now carries real dispatched-tool names from evidence
/// receipts (was: interaction action labels); interaction actions moved to
/// `signals`.
pub const SCHEMA_VERSION: u32 = 2;

/// The trace is a learning record, not a transcript — goals bound at this
/// length so a pasted document cannot dominate the file.
const MAX_GOAL_CHARS: usize = 240;
const MAX_FIELD_CHARS: usize = 120;
const MAX_LISTED: usize = 64;
/// Retrieval reads the whole file on every mission — keep it bounded.
/// ~8 MiB ≈ 16k typical records; past it the oldest half is dropped.
const MAX_TRACE_BYTES: u64 = 8 * 1024 * 1024;

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
    /// Distinct dispatched-tool names from the evidence session's receipts
    /// (falls back to capability-named interactions when no session bound
    /// the mission). Lifecycle actions like `MISSION_FLUX` or `PLAN_SEARCH`
    /// are not tools — they land in `signals`.
    pub tools: Vec<String>,
    /// Lifecycle/supervision actions observed in the mission's
    /// interactions (`PLAN_SEARCH`, `CLOUD_ATTEMPT_*`, `MISSION_FLUX`, ...).
    /// Kept for audit, kept out of `tools` so failure-history retrieval and
    /// briefs describe real capabilities only.
    #[serde(default)]
    pub signals: Vec<String>,
    /// Tier-0/1 action labels served during the mission (`reflex:*`
    /// receipts, non-citable). Lets the distill stage join served reflexes
    /// to outcomes — a reflex repeatedly on failed missions gets suppressed,
    /// not celebrated.
    #[serde(default)]
    pub reflex_served: Vec<String>,
    /// The executed plan's steps, when the mission ran plan search.
    /// Lets retrieval learn plan *shape*, not just goal text.
    #[serde(default)]
    pub plan_steps: Vec<String>,
    /// Chosen candidate's deliberation score.
    #[serde(default)]
    pub plan_score: Option<f32>,
    /// Top-two consensus when the gate measured it.
    #[serde(default)]
    pub plan_consensus: Option<f32>,
    /// 1-based index of the step that aborted the plan.
    #[serde(default)]
    pub failed_step: Option<u32>,
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
            signals: Vec::new(),
            reflex_served: Vec::new(),
            plan_steps: Vec::new(),
            plan_score: None,
            plan_consensus: None,
            failed_step: None,
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

    /// A success claim *backed by evidence*: the mission left at least one
    /// evidence receipt, so the verdict isn't bare prose. Teaching paths —
    /// reflex promotion, proven tools, plan exemplars — trust only
    /// verified wins; a receipt-free "SUCCESS" is a claim, not a lesson.
    pub fn verified(&self) -> bool {
        self.succeeded() && self.evidence_entries > 0
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
        let path = susi_dir.join("mission_traces.jsonl");
        // Bounded append: a long-lived workspace must not grow this file
        // forever — every retrieval reads it whole. Past MAX_TRACE_BYTES,
        // keep the newest half (oldest lines dropped, still-parseable file).
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_TRACE_BYTES {
                if let Ok(body) = std::fs::read(&path) {
                    let keep_from = body.len().saturating_sub((MAX_TRACE_BYTES / 2) as usize);
                    // Start at a line boundary so readers never see a
                    // partial first record.
                    let start = body[keep_from..]
                        .iter()
                        .position(|&b| b == b'\n')
                        .map(|i| keep_from + i + 1)
                        .unwrap_or(keep_from);
                    let _ = std::fs::write(&path, &body[start.min(body.len())..]);
                }
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
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

/// Content tokens of a goal — stopword- and punctuation-filtered.
fn goal_tokens(s: &str) -> std::collections::BTreeSet<String> {
    // Stopwords carry no intent signal — "the" alone must never make two
    // missions look similar.
    const STOPWORDS: &[&str] = &["the", "and", "for", "with", "this", "that", "from"];
    s.split_whitespace()
        .map(|t| {
            t.to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|t| t.len() > 2 && !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Weighted token overlap: each token contributes its IDF against the
/// trace corpus, so a shared rare token ("kubernetes") outweighs a shared
/// ubiquitous one ("deploy"). Plain Jaccard cannot tell "deploy api" from
/// "deploy database" when the corpus is full of "deploy".
fn token_overlap(
    a: &std::collections::BTreeSet<String>,
    b: &std::collections::BTreeSet<String>,
    df: &std::collections::BTreeMap<String, u32>,
    corpus: usize,
) -> f32 {
    let weight = |t: &String| {
        let d = df.get(t).copied().unwrap_or(0) as f32;
        ((corpus as f32 + 1.0) / (d + 1.0)).ln() + 1.0
    };
    let inter: f32 = a.intersection(b).map(weight).sum();
    let union: f32 = a.union(b).map(weight).sum();
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Similarity floor for a trace to count as "the same kind of mission".
const SIMILARITY_FLOOR: f32 = 0.15;
/// Recency decay constant for retrieval ranking — a trace this old still
/// counts, but past it the recency weight fades toward its 0.5 floor.
/// Old experience informs; it never outweighs fresh evidence forever.
const RECENCY_TAU_SECS: f64 = 30.0 * 86_400.0;

/// Retrieve the `limit` traces most similar to `goal` — the retrieval stage
/// of the loop: prior outcomes for the same kind of intent. Scored with
/// IDF-weighted token overlap computed over the corpus being searched,
/// then recency-weighted so newer experience outranks stale near-misses.
pub fn similar<'a>(goal: &str, traces: &'a [MissionTrace], limit: usize) -> Vec<&'a MissionTrace> {
    // Document frequency over the corpus: how many trace goals contain
    // each token. Corpus-rare tokens get the most weight in overlap.
    let mut df: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for t in traces {
        for tok in goal_tokens(&t.goal) {
            *df.entry(tok).or_insert(0) += 1;
        }
    }
    let newest = traces.iter().map(|t| t.timestamp).max().unwrap_or(0);
    let query = goal_tokens(goal);
    let mut scored: Vec<(f32, &'a MissionTrace)> = traces
        .iter()
        .map(|t| {
            (
                token_overlap(&query, &goal_tokens(&t.goal), &df, traces.len()),
                t,
            )
        })
        .filter(|(s, _)| *s >= SIMILARITY_FLOOR)
        .map(|(sim, t)| {
            // Recency weight: 0.5 + 0.5·e^(-age/τ) — the newest trace keeps
            // full similarity; an ancient one fades toward half, never to
            // zero (old lessons still count, just not above fresh ones).
            let age = newest.saturating_sub(t.timestamp) as f64;
            let weight = 0.5 + 0.5 * (-age / RECENCY_TAU_SECS).exp();
            (sim * weight as f32, t)
        })
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

/// Sorted unique content tokens — order- and punctuation-insensitive intent
/// identity, so "deploy the api" and "the api deploy" are the same habit.
fn token_signature(text: &str) -> String {
    text.split_whitespace()
        .map(|t| {
            t.to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|t| !t.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where an intent stands for reflex promotion. Promotion is earned by
/// verified outcomes — frequency alone is not evidence a reflex helps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionStatus {
    /// Enough verified successes and no unresolved failures.
    Promotable { successes: usize },
    /// A failure sits inside the recent window — this intent is an
    /// anti-pattern: veto promotion until fresh successes age it out.
    Vetoed { reason: String },
    /// Not enough verified success yet — defer, do not refuse.
    Insufficient { successes: usize, failures: usize },
}

/// Verified successes a recurring intent must show before promotion.
pub const MIN_PROMOTION_SUCCESSES: usize = 2;
/// A failure inside the last N traces for the intent vetoes promotion.
const RECENT_WINDOW: usize = 3;

/// Whether any trace records this exact intent (token-signature match) —
/// distinguishes "never observed" (legacy evidence applies) from "observed
/// but not yet proven" (promotion defers).
pub fn has_traces_for(traces: &[MissionTrace], intent: &str) -> bool {
    let signature = token_signature(intent);
    traces.iter().any(|t| token_signature(&t.goal) == signature)
}

pub fn promotion_status(traces: &[MissionTrace], intent: &str) -> PromotionStatus {
    let signature = token_signature(intent);
    let matching: Vec<&MissionTrace> = traces
        .iter()
        .filter(|t| token_signature(&t.goal) == signature)
        .collect();
    // Only evidence-backed wins count toward promotion — a bare "SUCCESS"
    // verdict with zero receipts is a claim, not proof a reflex helped.
    let successes = matching.iter().filter(|t| t.verified()).count();
    let failures = matching.len() - matching.iter().filter(|t| t.succeeded()).count();
    if let Some(failed) = matching
        .iter()
        .rev()
        .take(RECENT_WINDOW)
        .find(|t| !t.succeeded())
    {
        return PromotionStatus::Vetoed {
            reason: format!(
                "intent has an unresolved failure in its last {RECENT_WINDOW} traces (mission {}, outcome {})",
                failed.mission_id, failed.outcome
            ),
        };
    }
    if successes >= MIN_PROMOTION_SUCCESSES {
        PromotionStatus::Promotable { successes }
    } else {
        PromotionStatus::Insufficient {
            successes,
            failures,
        }
    }
}

/// The step texts that aborted similar failed missions — `failed_step` is
/// 1-based into `plan_steps`. Deliberation docks candidate steps that
/// echo a doomed step, so history steers plans around the step that
/// actually broke, not just away from tainted tools.
pub fn failed_steps(goal: &str, traces: &[MissionTrace], limit: usize) -> Vec<String> {
    similar(goal, traces, limit)
        .iter()
        .filter(|t| !t.succeeded())
        .filter_map(|t| {
            t.failed_step
                .and_then(|i| t.plan_steps.get(i.saturating_sub(1) as usize))
                .cloned()
        })
        .collect()
}

/// Tools that appeared in failed missions similar to `goal`, with the
/// number of failed missions each appeared on — repeated failures weigh
/// more than one-offs. Successes using the same tool don't clear it here
/// (the scorer weighs it); a tool only on failed runs is a real signal.
pub fn failing_tool_counts(
    goal: &str,
    traces: &[MissionTrace],
    limit: usize,
) -> std::collections::BTreeMap<String, u32> {
    let (mut failed, mut succeeded) = (
        std::collections::BTreeMap::<String, u32>::new(),
        std::collections::BTreeSet::<String>::new(),
    );
    for t in similar(goal, traces, limit) {
        if t.verified() {
            succeeded.extend(t.tools.iter().cloned());
        } else if t.succeeded() {
            // A bare success claim is neutral — it neither taints a tool
            // nor clears its failure record.
            continue;
        } else {
            // Count failed *missions*, not mentions — a tool invoked five
            // times in one failure is one data point, not five.
            for tool in t.tools.iter().collect::<std::collections::BTreeSet<_>>() {
                *failed.entry(tool.clone()).or_insert(0) += 1;
            }
        }
    }
    // A tool that also appears on successful similar missions is ambiguous —
    // only unambiguous failure carries the penalty.
    failed.retain(|tool, _| !succeeded.contains(tool));
    failed
}

/// The set view of `failing_tool_counts` — every tool with ≥1 unambiguous
/// failure on similar missions.
pub fn failing_tools(
    goal: &str,
    traces: &[MissionTrace],
    limit: usize,
) -> std::collections::BTreeSet<String> {
    failing_tool_counts(goal, traces, limit)
        .into_keys()
        .collect()
}

/// The positive counterpart of `failing_tools`: tools that appear only on
/// *successful* similar missions — the planner's "what worked here before"
/// signal. A tool that also failed is ambiguous and earns nothing.
pub fn proven_tools(
    goal: &str,
    traces: &[MissionTrace],
    limit: usize,
) -> std::collections::BTreeSet<String> {
    let (mut failed, mut succeeded) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for t in similar(goal, traces, limit) {
        // An unverified "success" is neutral: it can't prove a tool, but
        // its absence shouldn't taint one either.
        let target = if t.verified() {
            &mut succeeded
        } else if t.succeeded() {
            continue;
        } else {
            &mut failed
        };
        target.extend(t.tools.iter().cloned());
    }
    succeeded.difference(&failed).cloned().collect()
}

/// Fraction of similar missions that succeeded: `None` when nothing
/// similar exists (novel intent — callers should not treat absence as 0).
/// The single number routing and promotion want instead of recounting.
pub fn success_rate(goal: &str, traces: &[MissionTrace], limit: usize) -> Option<f32> {
    let neighbors = similar(goal, traces, limit);
    if neighbors.is_empty() {
        return None;
    }
    let wins = neighbors.iter().filter(|t| t.succeeded()).count();
    Some(wins as f32 / neighbors.len() as f32)
}

/// True when enough similar missions exist to judge (≥2) and fewer than
/// half succeeded — the neighborhood is unreliable: not vetoed, but
/// plan-search should demand candidate agreement before acting. A single
/// neighbor is too thin to condemn (one failure can be noise).
pub fn unreliable_neighborhood(goal: &str, traces: &[MissionTrace], limit: usize) -> bool {
    let neighbors = similar(goal, traces, limit);
    neighbors.len() >= 2 && neighbors.iter().filter(|t| t.succeeded()).count() * 2 < neighbors.len()
}

/// Whether plan scores predict outcomes: Pearson correlation of
/// `plan_score` with 0/1 success across traced plan-search missions.
/// `None` when too few scored missions exist to judge. The scorer is a
/// model like any other — if its ranking anti-correlates with verified
/// outcomes, it is steering plans wrong and should surface as a
/// negative number, not hide inside a plausible score.
pub fn plan_score_correlation(traces: &[MissionTrace]) -> Option<f32> {
    let scored: Vec<(f32, f32)> = traces
        .iter()
        .filter_map(|t| {
            t.plan_score
                .map(|s| (s, if t.succeeded() { 1.0 } else { 0.0 }))
        })
        .collect();
    if scored.len() < 4 {
        return None;
    }
    let n = scored.len() as f32;
    let mx = scored.iter().map(|(s, _)| s).sum::<f32>() / n;
    let my = scored.iter().map(|(_, o)| o).sum::<f32>() / n;
    let (mut cov, mut vx, mut vy) = (0.0f32, 0.0f32, 0.0f32);
    for (s, o) in &scored {
        let (dx, dy) = (s - mx, o - my);
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx == 0.0 || vy == 0.0 {
        return None;
    }
    Some(cov / (vx.sqrt() * vy.sqrt()))
}

/// Few-shot decomposition exemplar: the `plan_steps` of the most similar
/// successful trace. Only verified wins teach plan *shape* — a failed
/// mission's steps teach what to avoid, never what to copy. Empty when
/// nothing similar succeeded, so novel intents get no fabricated guidance.
pub fn proven_plan_brief(goal: &str, traces: &[MissionTrace]) -> String {
    let Some(t) = similar(goal, traces, 8)
        .into_iter()
        .find(|t| t.verified() && !t.plan_steps.is_empty())
    else {
        return String::new();
    };
    let steps = t
        .plan_steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n");
    format!("A similar goal succeeded with this plan:\n{steps}\n\n")
}

/// One-line history brief for prompt injection: what similar missions did
/// and how they ended. Empty when nothing similar exists.
pub fn history_brief(goal: &str, traces: &[MissionTrace], limit: usize) -> String {
    let lines: Vec<String> = similar(goal, traces, limit)
        .iter()
        .map(|t| {
            let step_note = t
                .failed_step
                .map(|s| format!(" [failed at step {s}]"))
                .unwrap_or_default();
            // Lifecycle signals worth planning around: a goal class the
            // governor refused once will refuse again; a cloud attempt
            // that already failed shouldn't be retried blind.
            let signal_note = if t.signals.iter().any(|s| s == "GOVERNANCE_BLOCK") {
                " [governance-blocked]"
            } else if t.signals.iter().any(|s| s == "CLOUD_ATTEMPT_FAILED") {
                " [cloud-attempt-failed]"
            } else {
                ""
            };
            format!(
                "- \"{}\" -> {} via {} (tools: {}){}{}",
                t.goal,
                t.outcome,
                t.route,
                t.tools.join(","),
                step_note,
                signal_note
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
        assert!((success_rate("deploy api service", &traces, 5).unwrap() - 1.0 / 3.0).abs() < 0.01);
        assert!(success_rate("unrelated xyz", &traces, 5).is_none());

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
    fn promotion_requires_verified_success_and_clean_window() {
        let t = |outcome: &str| {
            let mut t = MissionTrace::new("m", "deploy the api", outcome, "swarm");
            if t.succeeded() {
                t.evidence_entries = 1;
            }
            t
        };

        // No traces → insufficient, never promotable.
        assert!(matches!(
            promotion_status(&[], "deploy the api"),
            PromotionStatus::Insufficient { .. }
        ));

        // Word order and punctuation don't change intent identity.
        assert!(has_traces_for(&[t("COMPLETE")], "the api deploy"));
        assert!(!has_traces_for(&[t("COMPLETE")], "different goal"));

        // One success is not enough.
        assert!(matches!(
            promotion_status(&[t("COMPLETE")], "deploy the api"),
            PromotionStatus::Insufficient {
                successes: 1,
                failures: 0
            }
        ));

        // Two verified successes promote.
        assert_eq!(
            promotion_status(&[t("COMPLETE"), t("SUCCESS")], "deploy the api"),
            PromotionStatus::Promotable { successes: 2 }
        );

        // A success verdict with zero evidence entries is a claim, not a
        // win — it neither promotes nor counts as a failure.
        let unverified = {
            let mut t = MissionTrace::new("m", "deploy the api", "COMPLETE", "swarm");
            t.evidence_entries = 0;
            vec![t]
        };
        assert!(matches!(
            promotion_status(&unverified, "deploy the api"),
            PromotionStatus::Insufficient {
                successes: 0,
                failures: 0
            }
        ));

        // A failure inside the recent window vetoes even with history.
        let seq = vec![t("COMPLETE"), t("COMPLETE"), t("FAILED")];
        assert!(matches!(
            promotion_status(&seq, "deploy the api"),
            PromotionStatus::Vetoed { .. }
        ));

        // Failures older than the window are forgiven by fresh success.
        let recovered = vec![t("FAILED"), t("COMPLETE"), t("COMPLETE"), t("COMPLETE")];
        assert!(matches!(
            promotion_status(&recovered, "deploy the api"),
            PromotionStatus::Promotable { .. }
        ));
    }

    #[test]
    fn failing_tools_flags_only_unambiguous_failures() {
        let ws = workspace();
        // exec_command failed on deploys but also succeeded — ambiguous,
        // must not carry the veto. broken_tool only ever failed — it does.
        for (outcome, tools) in [
            ("FAILED", vec!["exec_command", "broken_tool"]),
            ("FAILED", vec!["exec_command", "broken_tool"]),
            ("COMPLETE", vec!["exec_command"]),
        ] {
            let mut t = MissionTrace::new("m", "deploy api service", outcome, "swarm");
            t.tools = tools.into_iter().map(String::from).collect();
            if t.succeeded() {
                t.evidence_entries = 1;
            }
            t.emit(ws.path()).unwrap();
        }
        let traces = read_all(ws.path());
        let failed = failing_tools("deploy api service", &traces, 8);
        assert!(failed.contains("broken_tool"), "{failed:?}");
        assert!(!failed.contains("exec_command"), "{failed:?}");
        // Counts scale with repeated failures for penalty weighting.
        let counts = failing_tool_counts("deploy api service", &traces, 8);
        assert_eq!(counts.get("broken_tool"), Some(&2));
        assert!(!counts.contains_key("exec_command"));
    }

    #[test]
    fn plan_fields_flow_into_brief_with_step_attribution() {
        let ws = workspace();
        let mut t = MissionTrace::new("m", "deploy api service", "FAILED", "swarm");
        t.tools = vec!["exec_command".into()];
        t.plan_steps = vec!["read config".into(), "deploy".into(), "verify".into()];
        t.plan_score = Some(0.72);
        t.plan_consensus = Some(0.9);
        t.failed_step = Some(2);
        t.emit(ws.path()).unwrap();
        let traces = read_all(ws.path());
        let got = &traces[0];
        assert_eq!(got.failed_step, Some(2));
        assert_eq!(got.plan_steps.len(), 3);
        let brief = history_brief("deploy api service", &traces, 3);
        assert!(brief.contains("failed at step 2"), "{brief}");
    }

    #[test]
    fn brief_flags_governance_blocked_and_cloud_failed_history() {
        let ws = workspace();
        let mut blocked = MissionTrace::new("b", "wipe the production disk", "FAILED", "swarm");
        blocked.signals = vec!["GOVERNANCE_BLOCK".into()];
        let mut cloud = MissionTrace::new("c", "wipe the production disk", "FAILED", "swarm");
        cloud.signals = vec!["CLOUD_ATTEMPT_FAILED".into()];
        blocked.emit(ws.path()).unwrap();
        cloud.emit(ws.path()).unwrap();
        let traces = read_all(ws.path());
        let brief = history_brief("wipe the production disk", &traces, 8);
        assert!(brief.contains("[governance-blocked]"), "{brief}");
        assert!(brief.contains("[cloud-attempt-failed]"), "{brief}");
        // Ordinary failure signals carry neither annotation.
        let mut plain = MissionTrace::new("p", "deploy api service", "FAILED", "swarm");
        plain.signals = vec!["PLAN_SEARCH".into()];
        let plain_brief = history_brief("deploy api service", &[plain], 8);
        assert!(!plain_brief.contains("governance-blocked"));
        assert!(!plain_brief.contains("cloud-attempt-failed"));
    }

    #[test]
    fn proven_plan_brief_only_teaches_from_successes() {
        let ws = workspace();
        // Most similar trace FAILED — it must not become the exemplar.
        let mut bad = MissionTrace::new("bad", "deploy api service", "FAILED", "swarm");
        bad.plan_steps = vec!["guess blindly".into()];
        let mut good = MissionTrace::new("good", "deploy api service", "SUCCESS", "swarm");
        good.plan_steps = vec!["read config".into(), "deploy".into(), "verify".into()];
        good.evidence_entries = 1;
        bad.emit(ws.path()).unwrap();
        good.emit(ws.path()).unwrap();
        let traces = read_all(ws.path());
        let brief = proven_plan_brief("deploy api service", &traces);
        assert!(brief.contains("1. read config"), "{brief}");
        assert!(brief.contains("3. verify"), "{brief}");
        assert!(!brief.contains("guess blindly"), "{brief}");
        // Novel goal: no exemplar, no fabricated guidance.
        assert!(proven_plan_brief("unrelated never-seen task", &traces).is_empty());
    }

    #[test]
    fn plan_score_correlation_flags_a_scorer_that_predicts_backwards() {
        let ws = workspace();
        // A broken scorer: every plan that scored high failed, every plan
        // that scored low succeeded → strong negative correlation.
        for (id, score, outcome) in [
            ("a", 0.9, "FAILED"),
            ("b", 0.8, "FAILED"),
            ("c", 0.3, "SUCCESS"),
            ("d", 0.2, "SUCCESS"),
            ("e", 0.1, "SUCCESS"),
        ] {
            let mut t = MissionTrace::new(id, "x", outcome, "swarm");
            t.plan_score = Some(score);
            t.emit(ws.path()).unwrap();
        }
        let traces = read_all(ws.path());
        let r = plan_score_correlation(&traces).unwrap();
        assert!(r < -0.5, "expected strong negative, got {r}");
        // Too few scored missions → no verdict.
        assert_eq!(plan_score_correlation(&traces[..3]), None);
        // No scored missions at all → no verdict.
        let bare = MissionTrace::new("n", "x", "SUCCESS", "swarm");
        assert_eq!(plan_score_correlation(&[bare]), None);
    }

    #[test]
    fn unreliable_neighborhood_needs_two_neighbors_and_below_half() {
        let ws = workspace();
        for (id, outcome) in [
            ("a", "FAILED"),
            ("b", "FAILED"),
            ("c", "SUCCESS"),
            ("d", "FAILED"),
        ] {
            MissionTrace::new(id, "deploy api service", outcome, "swarm")
                .emit(ws.path())
                .unwrap();
        }
        let traces = read_all(ws.path());
        // 4 similar, 1 success → rate 0.25 → unreliable.
        assert!(unreliable_neighborhood("deploy api service", &traces, 8));
        // One success, one failure → rate 0.5, not below half.
        let two = &traces[1..3];
        assert!(!unreliable_neighborhood("deploy api service", two, 8));
        // Single neighbor — too thin to condemn.
        assert!(!unreliable_neighborhood(
            "deploy api service",
            &traces[..1],
            8
        ));
        // Novel intent — nothing to judge.
        assert!(!unreliable_neighborhood("never seen intent", &traces, 8));
    }

    #[test]
    fn idf_ranks_rare_shared_tokens_over_common_ones() {
        let ws = workspace();
        for (id, goal) in [
            ("a", "deploy database backup"),
            ("b", "deploy cache layer"),
            ("c", "kubernetes pod status"),
            ("d", "deploy network rules"),
        ] {
            MissionTrace::new(id, goal, "DONE", "swarm")
                .emit(ws.path())
                .unwrap();
        }
        let traces = read_all(ws.path());
        // Plain Jaccard ties "deploy database backup" and "kubernetes pod
        // status" at one shared token each. IDF sees "deploy" is corpus-
        // ubiquitous and "kubernetes" rare — the rare match wins and the
        // common-token match falls below the floor.
        let hits = similar("deploy kubernetes service", &traces, 8);
        assert_eq!(hits[0].goal, "kubernetes pod status");
        assert_eq!(
            hits.len(),
            1,
            "{:?}",
            hits.iter().map(|t| &t.goal).collect::<Vec<_>>()
        );
    }

    #[test]
    fn recency_decay_demotes_stale_near_matches() {
        let ws = workspace();
        // Ancient trace shares two goal tokens; fresh one shares only the
        // rarest — raw similarity favors the ancient trace, but its age
        // halves its weight and the fresh one wins.
        let mut old = MissionTrace::new("old", "deploy kubernetes pod status", "SUCCESS", "swarm");
        old.timestamp = 1;
        let mut fresh = MissionTrace::new("new", "kubernetes only", "SUCCESS", "swarm");
        fresh.timestamp = 1_800_000_000;
        old.emit(ws.path()).unwrap();
        fresh.emit(ws.path()).unwrap();
        let traces = read_all(ws.path());
        let hits = similar("deploy kubernetes service", &traces, 8);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].mission_id,
            "new",
            "{:?}",
            hits.iter().map(|t| &t.goal).collect::<Vec<_>>()
        );
    }

    #[test]
    fn failed_steps_extracts_only_doomed_step_texts() {
        let ws = workspace();
        let mut failed = MissionTrace::new("f", "deploy api service", "FAILED", "swarm");
        failed.plan_steps = vec!["read config".into(), "deploy".into(), "verify".into()];
        failed.failed_step = Some(2);
        let mut ok = MissionTrace::new("o", "deploy api service", "SUCCESS", "swarm");
        ok.plan_steps = vec!["read config".into(), "deploy".into()];
        ok.failed_step = None;
        // Failure with no failed_step attribution contributes nothing.
        let mut unattributed = MissionTrace::new("u", "deploy api service", "FAILED", "swarm");
        unattributed.plan_steps = vec!["deploy".into()];
        failed.emit(ws.path()).unwrap();
        ok.emit(ws.path()).unwrap();
        unattributed.emit(ws.path()).unwrap();
        let traces = read_all(ws.path());
        let doomed = failed_steps("deploy api service", &traces, 8);
        assert_eq!(doomed, vec!["deploy".to_string()]);
    }

    #[test]
    fn oversized_log_rotates_to_newest_parseable_half() {
        let ws = workspace();
        let susi = ws.path().join(".susi");
        std::fs::create_dir_all(&susi).unwrap();
        // Pre-seed a file just over the cap; the next emit must rotate it.
        let mut big = vec![b'x'; (MAX_TRACE_BYTES + 64) as usize];
        big.push(b'\n');
        std::fs::write(susi.join("mission_traces.jsonl"), &big).unwrap();
        MissionTrace::new("m-new", "goal", "COMPLETE", "swarm")
            .emit(ws.path())
            .unwrap();
        let parsed = read_all(ws.path());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].mission_id, "m-new");
        let size = std::fs::metadata(susi.join("mission_traces.jsonl"))
            .unwrap()
            .len();
        assert!(size < MAX_TRACE_BYTES, "{size}");
    }

    #[test]
    fn proven_tools_flags_only_unambiguous_successes() {
        let ws = workspace();
        for (outcome, tools) in [
            ("COMPLETE", vec!["exec_command", "cargo_build"]),
            ("COMPLETE", vec!["cargo_build"]),
            ("FAILED", vec!["exec_command", "flaky_tool", "flaky_tool"]),
        ] {
            let mut t = MissionTrace::new("m", "build rust crate", outcome, "swarm");
            t.tools = tools.into_iter().map(String::from).collect();
            if t.succeeded() {
                t.evidence_entries = 1;
            }
            t.emit(ws.path()).unwrap();
        }
        let traces = read_all(ws.path());
        // Repeated invocations in one mission count as one failed mission —
        // the count tracks missions the tool appeared on, not mentions.
        let counts = failing_tool_counts("build rust crate", &traces, 8);
        assert_eq!(counts.get("flaky_tool"), Some(&1));
        let proven = proven_tools("build rust crate", &traces, 8);
        // cargo_build only ever succeeded; exec_command also failed once.
        assert!(proven.contains("cargo_build"), "{proven:?}");
        assert!(!proven.contains("exec_command"), "{proven:?}");
        assert!(!proven.contains("flaky_tool"));
    }

    #[test]
    fn v1_lines_without_signals_still_parse() {
        let ws = workspace();
        let susi = ws.path().join(".susi");
        std::fs::create_dir_all(&susi).unwrap();
        // A schema-1 record: no `signals` field at all.
        std::fs::write(
            susi.join("mission_traces.jsonl"),
            r#"{"schema_version":1,"mission_id":"old","goal":"deploy api","outcome":"COMPLETE","route":"swarm","tools":["exec_command"],"agents":[],"evidence_entries":2,"duration_secs":null,"timestamp":1}
"#,
        )
        .unwrap();
        let parsed = read_all(ws.path());
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].signals.is_empty());
        assert_eq!(parsed[0].tools, vec!["exec_command".to_string()]);
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
