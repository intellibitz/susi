//! Scoreboard of external agents by task class.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentScore {
    pub successes: u64,
    pub failures: u64,
    pub avg_latency_ms: f64,
}

impl AgentScore {
    #[must_use]
    pub fn rate(&self) -> f64 {
        let n = self.successes + self.failures;
        if n == 0 {
            0.0
        } else {
            self.successes as f64 / n as f64
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentScoreboard {
    /// task_class -> agent_id -> score
    pub by_class: BTreeMap<String, BTreeMap<String, AgentScore>>,
}

impl AgentScoreboard {
    pub fn record(&mut self, task_class: &str, agent: &str, success: bool, latency_ms: u64) {
        let entry = self
            .by_class
            .entry(task_class.into())
            .or_default()
            .entry(agent.into())
            .or_insert(AgentScore {
                successes: 0,
                failures: 0,
                avg_latency_ms: 0.0,
            });
        let n = entry.successes + entry.failures;
        entry.avg_latency_ms =
            (entry.avg_latency_ms * n as f64 + latency_ms as f64) / (n + 1) as f64;
        if success {
            entry.successes += 1;
        } else {
            entry.failures += 1;
        }
    }

    #[must_use]
    pub fn best(&self, task_class: &str) -> Option<&str> {
        let m = self.by_class.get(task_class)?;
        m.iter()
            .max_by(|a, b| {
                a.1.rate()
                    .partial_cmp(&b.1.rate())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(k, _)| k.as_str())
    }
}

#[cfg(test)]
mod agent_scoreboard_tests {
    use super::*;

    #[test]
    fn agent_scoreboard_ranks_by_success_rate() {
        let mut s = AgentScoreboard::default();
        s.record("code", "cursor", true, 100);
        s.record("code", "cursor", true, 120);
        s.record("code", "aider", true, 80);
        s.record("code", "aider", false, 90);
        assert_eq!(s.best("code"), Some("cursor"));
        assert!(s.best("chat").is_none());
    }
}
