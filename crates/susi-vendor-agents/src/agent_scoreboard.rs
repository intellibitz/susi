//! Scoreboard of external agents by task class — the same shape the
//! brain keeps for models: attempts, successes, mean duration and mean
//! cost per (agent, task-class) cell, persisted so routing can prefer the
//! best-evidenced agent. Surfaced by `susi agents scoreboard`.

use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One finished delegated run's measurable outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    /// External agent id ("cursor", "devin", …).
    pub agent: String,
    /// Brain task class ("code", "research", …).
    pub task_class: String,
    pub success: bool,
    pub duration_ms: u64,
    /// USD the run cost (0 for local/free agents).
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub unix_ms: u64,
}

/// Aggregate row for `susi agents scoreboard`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentScore {
    pub agent: String,
    pub task_class: String,
    pub attempts: u64,
    pub successes: u64,
    /// success / attempt — the ranking signal.
    pub success_rate: f64,
    pub mean_duration_ms: f64,
    pub mean_cost_usd: f64,
    /// Mean cost per *successful* run — None when nothing succeeded.
    pub cost_per_success: Option<f64>,
}

/// Persisted ledger of external-agent outcomes.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Scoreboard {
    #[serde(default)]
    runs: Vec<AgentRun>,
}

impl Scoreboard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, run: AgentRun) {
        self.runs.push(run);
    }

    /// Aggregate every (agent, class) cell, best success-rate first.
    #[must_use]
    pub fn scores(&self) -> Vec<AgentScore> {
        #[derive(Default)]
        struct Acc {
            attempts: u64,
            successes: u64,
            dur: u128,
            cost: f64,
            cost_success: f64,
            n_success: u64,
        }
        let mut cells: BTreeMap<(String, String), Acc> = BTreeMap::new();
        for r in &self.runs {
            let a = cells
                .entry((r.agent.clone(), r.task_class.clone()))
                .or_default();
            a.attempts += 1;
            a.dur += u128::from(r.duration_ms);
            a.cost += r.cost_usd;
            if r.success {
                a.successes += 1;
                a.n_success += 1;
                a.cost_success += r.cost_usd;
            }
        }
        let mut out: Vec<AgentScore> = cells
            .into_iter()
            .map(|((agent, task_class), a)| {
                let rate = a.successes as f64 / a.attempts.max(1) as f64;
                AgentScore {
                    agent,
                    task_class,
                    attempts: a.attempts,
                    successes: a.successes,
                    success_rate: rate,
                    mean_duration_ms: a.dur as f64 / a.attempts.max(1) as f64,
                    mean_cost_usd: a.cost / a.attempts.max(1) as f64,
                    cost_per_success: (a.n_success > 0)
                        .then(|| a.cost_success / a.n_success as f64),
                }
            })
            .collect();
        out.sort_by(|x, y| {
            y.success_rate
                .partial_cmp(&x.success_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(x.agent.cmp(&y.agent))
        });
        out
    }

    #[must_use]
    fn scores_by_class(&self, task_class: &str, min_attempts: u64) -> Vec<AgentScore> {
        self.scores()
            .into_iter()
            .filter(|s| s.task_class == task_class && s.attempts >= min_attempts)
            .collect()
    }

    /// Owning variant of `best_for` — the score row by value.
    #[must_use]
    pub fn best_agent_for(&self, task_class: &str, min_attempts: u64) -> Option<AgentScore> {
        self.scores_by_class(task_class, min_attempts)
            .into_iter()
            .next()
    }

    /// Render the `susi agents scoreboard` table.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(
            "agent           class      attempts  ok%    mean ms   mean $   $/success\n",
        );
        for s in self.scores() {
            out.push_str(&format!(
                "{:<15} {:<10} {:>8} {:>5.0}% {:>9.0} {:>8.4} {}\n",
                s.agent,
                s.task_class,
                s.attempts,
                s.success_rate * 100.0,
                s.mean_duration_ms,
                s.mean_cost_usd,
                s.cost_per_success
                    .map_or("-".to_string(), |c| format!("{c:.4}")),
            ));
        }
        out
    }

    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }

    /// # Errors
    /// [`EaiError::io`] on encode/write failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(agent: &str, class: &str, ok: bool, ms: u64, usd: f64) -> AgentRun {
        AgentRun {
            agent: agent.to_string(),
            task_class: class.to_string(),
            success: ok,
            duration_ms: ms,
            cost_usd: usd,
            unix_ms: 0,
        }
    }

    #[test]
    fn agent_scoreboard_aggregates_per_agent_class() {
        let mut b = Scoreboard::new();
        b.record(run("cursor", "code", true, 1000, 0.10));
        b.record(run("cursor", "code", true, 3000, 0.10));
        b.record(run("cursor", "code", false, 500, 0.05));
        b.record(run("devin", "code", true, 2000, 0.20));
        let scores = b.scores();
        let cursor = scores.iter().find(|s| s.agent == "cursor").unwrap();
        assert_eq!(cursor.attempts, 3);
        assert_eq!(cursor.successes, 2);
        assert!((cursor.success_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(cursor.mean_duration_ms, 1500.0);
    }

    #[test]
    fn agent_scoreboard_ranks_by_success_rate() {
        let mut b = Scoreboard::new();
        b.record(run("weak", "research", false, 100, 0.0));
        b.record(run("weak", "research", true, 100, 0.0));
        b.record(run("strong", "research", true, 500, 0.0));
        b.record(run("strong", "research", true, 700, 0.0));
        let scores = b.scores();
        assert_eq!(scores[0].agent, "strong");
    }

    #[test]
    fn agent_scoreboard_cost_per_success() {
        let mut b = Scoreboard::new();
        b.record(run("paid", "code", true, 0, 0.40));
        b.record(run("paid", "code", true, 0, 0.60));
        b.record(run("paid", "code", false, 0, 0.30)); // failed runs cost but don't divide
        let s = b.scores().into_iter().find(|s| s.agent == "paid").unwrap();
        assert!((s.cost_per_success.unwrap() - 0.50).abs() < 1e-9);
        assert!((s.mean_cost_usd - 1.3 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn agent_scoreboard_best_for_respects_min_attempts() {
        let mut b = Scoreboard::new();
        b.record(run("lucky", "code", true, 0, 0.0));
        b.record(run("proven", "code", true, 0, 0.0));
        b.record(run("proven", "code", true, 0, 0.0));
        b.record(run("proven", "code", false, 0, 0.0));
        // min_attempts=3: "lucky" (1 attempt, 100%) must not beat "proven"
        let best = b.best_agent_for("code", 3).unwrap();
        assert_eq!(best.agent, "proven");
        assert!(b.best_agent_for("docs", 1).is_none());
    }

    #[test]
    fn agent_scoreboard_render_has_columns() {
        let mut b = Scoreboard::new();
        b.record(run("cursor", "code", true, 100, 0.5));
        let t = b.render();
        assert!(t.contains("ok%"));
        assert!(t.contains("cursor"));
        assert!(t.contains("$/success"));
    }

    #[test]
    fn agent_scoreboard_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("scoreboard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.json");
        let mut b = Scoreboard::new();
        b.record(run("a", "c", true, 1, 0.0));
        b.save(&p).unwrap();
        assert_eq!(Scoreboard::load(&p).unwrap().runs.len(), 1);
        assert!(Scoreboard::load(&dir.join("none.json"))
            .unwrap()
            .runs
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
