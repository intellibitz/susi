//! Mission scheduler: priority-scored admission control for swarm dispatch.
//!
//! `dispatch_explosive_swarm` used to run every recruited agent in one rayon
//! batch with no ordering and no cap — the hardware-derived concurrency limit
//! only loosely bounded fleet *synthesis*. This module gives dispatch a real
//! admission decision: agents are scored, the top `max_concurrent_agents`
//! run, and the rest are deferred (logged, never executed).
//!
//! Score = `0.5 * learned_rank + 0.35 * intent_match + 0.15 * urgency_affinity`
//!
//! - `learned_rank`: the agent's persisted `AgentMetaRegistry` rank — missions
//!   already update it via `rank_delta_for_output`, so scheduling improves
//!   from real outcome history rather than a static table.
//! - `intent_match`: Jaccard overlap between goal tokens and the agent's
//!   registered `semantic_anchors`/`categories`/name.
//! - `urgency_affinity`: when the goal carries failure/security/research
//!   markers, agents in the matching affinity class get a bounded boost.
//!
//! Governance agents (Safety/Security) are not scheduled here — they run
//! unconditionally *before* this ordering in `dispatch_explosive_swarm`.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use susi_core::{AgentMetaRegistry, GawdAgent};

/// How many recent schedule decisions to retain for `recent_decisions`.
const DECISION_LOG_CAP: usize = 32;

const FAILURE_MARKERS: &[&str] = &[
    "fix",
    "error",
    "fail",
    "broken",
    "down",
    "crash",
    "urgent",
    "regression",
    "stuck",
];
const SECURITY_MARKERS: &[&str] = &[
    "security", "leak", "secret", "vulnerab", "exploit", "token", "breach",
];
const RESEARCH_MARKERS: &[&str] = &[
    "search", "find", "where", "latest", "docs", "research", "lookup",
];

struct AffinityClass {
    markers: &'static [&'static str],
    agents: &'static [&'static str],
}

const AFFINITIES: &[AffinityClass] = &[
    AffinityClass {
        markers: FAILURE_MARKERS,
        agents: &["SelfHealingAgent", "DevOpsAgent", "ResourceArbitratorAgent"],
    },
    AffinityClass {
        markers: SECURITY_MARKERS,
        agents: &["SecurityAgent", "SafetyAgent", "EpistemicAuditorAgent"],
    },
    AffinityClass {
        markers: RESEARCH_MARKERS,
        agents: &["SearchAgent", "LibraryScoutAgent", "ContextAgent"],
    },
];

/// One agent's admission decision for a mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledEntry {
    pub name: String,
    pub score: f32,
    pub learned_rank: f32,
    pub intent_match: f32,
    pub urgency_boost: f32,
    /// `true` = admitted to the dispatch wave, `false` = deferred this mission.
    pub admitted: bool,
}

/// Record of one dispatch decision — published on the global typed bus and
/// retained in a bounded in-process log for inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDecision {
    /// Truncated goal text (scheduling telemetry, not a prompt store).
    pub goal: String,
    pub entries: Vec<ScheduledEntry>,
    pub timestamp: u64,
}

pub struct MissionScheduler;

fn decision_log() -> &'static Mutex<VecDeque<ScheduleDecision>> {
    static LOG: OnceLock<Mutex<VecDeque<ScheduleDecision>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn goal_tokens(goal: &str) -> HashSet<String> {
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect()
}

impl MissionScheduler {
    /// Fraction of the agent's vocabulary (anchors + categories + name parts)
    /// that appears in the goal tokens.
    fn intent_match(agent_name: &str, tokens: &HashSet<String>) -> f32 {
        let registry = AgentMetaRegistry::global();
        let profile_vocab: HashSet<String> = registry
            .list_agents()
            .into_iter()
            .find(|p| p.name == agent_name)
            .map(|p| {
                p.semantic_anchors
                    .iter()
                    .chain(p.categories.iter())
                    .chain(std::iter::once(&p.name))
                    .flat_map(|s| goal_tokens(s))
                    .collect()
            })
            .unwrap_or_else(|| goal_tokens(agent_name));

        if profile_vocab.is_empty() {
            return 0.0;
        }
        let hits = profile_vocab.intersection(tokens).count();
        hits as f32 / profile_vocab.len() as f32
    }

    /// Bounded bonus when the goal's urgency markers intersect the agent's
    /// affinity class. 0.0 or 1.0 — presence, not magnitude.
    fn urgency_boost(agent_name: &str, tokens: &HashSet<String>) -> f32 {
        for class in AFFINITIES {
            if class.agents.contains(&agent_name)
                && class.markers.iter().any(|m| tokens.contains(*m))
            {
                return 1.0;
            }
        }
        0.0
    }

    /// Score + order agents for dispatch. Returns the full scored list
    /// (admitted first, then deferred) — the caller slices at its cap.
    pub fn plan(goal: &str, agents: &[Arc<dyn GawdAgent>]) -> Vec<ScheduledEntry> {
        let tokens = goal_tokens(goal);
        let mut entries: Vec<ScheduledEntry> = agents
            .iter()
            .map(|agent| {
                let name = agent.name();
                let rank = agent.rank();
                let intent = Self::intent_match(&name, &tokens);
                let urgency = Self::urgency_boost(&name, &tokens);
                ScheduledEntry {
                    name,
                    score: 0.5 * rank + 0.35 * intent + 0.15 * urgency,
                    learned_rank: rank,
                    intent_match: intent,
                    urgency_boost: urgency,
                    admitted: false,
                }
            })
            .collect();
        entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        entries
    }

    /// Admission control: score the fleet, admit at most `cap` agents, defer
    /// the rest. Returns `(admitted_agents, decision)`; the decision is also
    /// recorded and published on the global typed bus.
    pub fn schedule(
        goal: &str,
        agents: Vec<Arc<dyn GawdAgent>>,
        cap: usize,
    ) -> (Vec<Arc<dyn GawdAgent>>, ScheduleDecision) {
        let mut entries = Self::plan(goal, &agents);
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.admitted = i < cap;
        }

        let by_name: std::collections::HashMap<String, Arc<dyn GawdAgent>> =
            agents.into_iter().map(|a| (a.name(), a)).collect();
        let admitted: Vec<Arc<dyn GawdAgent>> = entries
            .iter()
            .filter(|e| e.admitted)
            .filter_map(|e| by_name.get(&e.name).cloned())
            .collect();

        let decision = ScheduleDecision {
            goal: goal.chars().take(200).collect(),
            entries,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };

        {
            let mut log = decision_log().lock().unwrap();
            if log.len() >= DECISION_LOG_CAP {
                log.pop_front();
            }
            log.push_back(decision.clone());
        }
        susi_core::bus::global_bus().publish(decision.clone());

        (admitted, decision)
    }

    /// Most recent dispatch decisions (newest last), for status/inspection.
    pub fn recent_decisions() -> Vec<ScheduleDecision> {
        decision_log().lock().unwrap().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAgent {
        name: &'static str,
        rank: f32,
    }
    impl GawdAgent for TestAgent {
        fn name(&self) -> String {
            self.name.to_string()
        }
        fn rank(&self) -> f32 {
            self.rank
        }
        fn execute(
            &self,
            _goal: &str,
            _workspace: &std::path::Path,
            _blackboard: &susi_core::MissionBlackboard,
        ) -> susi_error::EaiResult<String> {
            Ok("ok".into())
        }
    }

    fn agent(name: &'static str, rank: f32) -> Arc<dyn GawdAgent> {
        Arc::new(TestAgent { name, rank })
    }

    #[test]
    fn higher_rank_orders_first() {
        let agents = vec![agent("Low", 0.1), agent("High", 0.9), agent("Mid", 0.5)];
        let plan = MissionScheduler::plan("do something", &agents);
        assert_eq!(plan[0].name, "High");
        assert_eq!(plan[2].name, "Low");
    }

    #[test]
    fn cap_defers_low_priority_agents() {
        let agents = vec![agent("A", 0.9), agent("B", 0.8), agent("C", 0.1)];
        let (admitted, decision) = MissionScheduler::schedule("goal", agents, 2);
        assert_eq!(admitted.len(), 2);
        let deferred: Vec<_> = decision.entries.iter().filter(|e| !e.admitted).collect();
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].name, "C");
    }

    #[test]
    fn urgency_boost_promotes_affinity_agent() {
        // SelfHealingAgent's learned rank is lower, but a "crash" goal should
        // push it ahead of a higher-ranked unrelated agent.
        let agents = vec![agent("UnrelatedAgent", 0.9), agent("SelfHealingAgent", 0.5)];
        let plan = MissionScheduler::plan("the daemon crash keeps failing, urgent fix", &agents);
        assert_eq!(plan[0].name, "SelfHealingAgent");
        assert!(plan[0].urgency_boost > 0.0);
    }

    #[test]
    fn schedule_records_and_publishes_decision() {
        let rx = susi_core::bus::global_bus().subscribe::<ScheduleDecision>();
        let before = MissionScheduler::recent_decisions().len();
        let _ = MissionScheduler::schedule("a goal", vec![agent("X", 0.5)], 1);
        assert!(MissionScheduler::recent_decisions().len() > before);
        // Other tests share the process-global bus — find *our* decision.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match rx.try_recv() {
                Ok(d) if d.entries.iter().any(|e| e.name == "X") => break,
                Ok(_) => continue,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(e) => panic!("decision must reach bus subscribers: {}", e),
            }
        }
    }
}
