//! Route tasks between the native swarm and external agents by evidence.
//!
//! The privacy and budget filters run FIRST — a task that may not leave
//! the machine, or cannot afford an external agent, never reaches the
//! scoreboard. Only then does evidence decide: the best-scored agent for
//! the task class wins, and the swarm is the fallback whenever the
//! evidence is missing, thin or too expensive.

use susi_vendor_agents::agent_scoreboard::{AgentScore, Scoreboard};

/// Whether a task's payload may leave this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    /// Must run locally — secrets, regulated data, air-gapped posture.
    LocalOnly,
    /// May be delegated to external agents.
    MayDelegate,
}

/// Constraints evaluated before evidence.
#[derive(Debug, Clone)]
pub struct RoutePolicy {
    pub privacy: Privacy,
    /// Remaining USD budget for external delegation; `None` = unlimited.
    /// Compared against the candidate's cost-per-success (mean cost when
    /// no success has been recorded yet).
    pub budget_usd: Option<f64>,
    /// Evidence floor: minimum recorded attempts before an agent's
    /// score counts. Below it the record is noise, not signal.
    pub min_attempts: u64,
    /// Whether the native swarm can execute at all (peers/executor up).
    pub swarm_available: bool,
}

impl Default for RoutePolicy {
    fn default() -> Self {
        Self {
            privacy: Privacy::MayDelegate,
            budget_usd: None,
            min_attempts: 3,
            swarm_available: true,
        }
    }
}

/// Where a task goes.
#[derive(Debug)]
pub enum RouteTarget {
    /// Execute in the native swarm.
    Swarm {
        /// Why the swarm was chosen over delegation.
        reason: &'static str,
    },
    /// Delegate to this external agent, carrying its evidence row.
    External { agent: String, score: AgentScore },
    /// Nothing can take it (e.g. swarm down and privacy forbids egress).
    Unroutable {
        /// Why no executor qualified.
        reason: &'static str,
    },
}

/// Decide where `task_class` runs under `policy`, given the recorded
/// outcomes in `board`.
#[must_use]
pub fn route(task_class: &str, policy: &RoutePolicy, board: &Scoreboard) -> RouteTarget {
    // Filter 1: privacy — local-only never leaves, regardless of evidence.
    if policy.privacy == Privacy::LocalOnly {
        return if policy.swarm_available {
            RouteTarget::Swarm {
                reason: "local-only privacy",
            }
        } else {
            RouteTarget::Unroutable {
                reason: "privacy forbids egress and swarm is unavailable",
            }
        };
    }

    // Filter 2+3: evidence and budget — a candidate must have enough
    // attempts, at least one success, and fit the remaining budget.
    let candidate = board
        .best_agent_for(task_class, policy.min_attempts)
        .filter(|s| s.successes > 0)
        .filter(|s| match policy.budget_usd {
            None => true,
            Some(budget) => s.cost_per_success.unwrap_or(s.mean_cost_usd) <= budget,
        });

    match candidate {
        Some(score) => RouteTarget::External {
            agent: score.agent.clone(),
            score,
        },
        None if policy.swarm_available => RouteTarget::Swarm {
            reason: "no qualifying external agent (evidence or budget)",
        },
        None => RouteTarget::Unroutable {
            reason: "swarm unavailable and no qualifying external agent",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_vendor_agents::agent_scoreboard::AgentRun;

    fn run(agent: &str, class: &str, ok: bool, usd: f64) -> AgentRun {
        AgentRun {
            agent: agent.to_string(),
            task_class: class.to_string(),
            success: ok,
            duration_ms: 100,
            cost_usd: usd,
            unix_ms: 0,
        }
    }

    fn board_with(agent: &str, class: &str, ok_runs: u64, fails: u64, usd: f64) -> Scoreboard {
        let mut b = Scoreboard::new();
        for _ in 0..ok_runs {
            b.record(run(agent, class, true, usd));
        }
        for _ in 0..fails {
            b.record(run(agent, class, false, usd));
        }
        b
    }

    #[test]
    fn agent_routing_local_only_never_delegates() {
        let b = board_with("cursor", "code", 10, 0, 0.0);
        let p = RoutePolicy {
            privacy: Privacy::LocalOnly,
            ..Default::default()
        };
        assert!(matches!(
            route("code", &p, &b),
            RouteTarget::Swarm {
                reason: "local-only privacy"
            }
        ));
    }

    #[test]
    fn agent_routing_picks_best_evidenced_agent() {
        let mut b = board_with("weak", "code", 2, 1, 0.0);
        for _ in 0..5 {
            b.record(run("strong", "code", true, 0.0));
        }
        match route("code", &RoutePolicy::default(), &b) {
            RouteTarget::External { agent, score } => {
                assert_eq!(agent, "strong");
                assert_eq!(score.attempts, 5);
            }
            RouteTarget::Swarm { .. } | RouteTarget::Unroutable { .. } => {
                panic!("strong agent should win")
            }
        }
    }

    #[test]
    fn agent_routing_falls_back_to_swarm_without_evidence() {
        let b = Scoreboard::new(); // empty ledger
        assert!(matches!(
            route("code", &RoutePolicy::default(), &b),
            RouteTarget::Swarm { .. }
        ));
    }

    #[test]
    fn agent_routing_thin_evidence_is_not_signal() {
        let b = board_with("lucky", "code", 1, 0, 0.0); // 1 attempt < min 3
        assert!(matches!(
            route("code", &RoutePolicy::default(), &b),
            RouteTarget::Swarm { .. }
        ));
    }

    #[test]
    fn agent_routing_budget_blocks_expensive_agent() {
        let b = board_with("pricey", "code", 5, 0, 2.0); // $2/success
        let p = RoutePolicy {
            budget_usd: Some(1.0),
            ..Default::default()
        };
        assert!(matches!(route("code", &p, &b), RouteTarget::Swarm { .. }));
        let rich = RoutePolicy {
            budget_usd: Some(5.0),
            ..Default::default()
        };
        assert!(matches!(
            route("code", &rich, &b),
            RouteTarget::External { .. }
        ));
    }

    #[test]
    fn agent_routing_zero_success_agent_never_chosen() {
        let b = board_with("all-fail", "code", 0, 10, 0.0);
        assert!(matches!(
            route("code", &RoutePolicy::default(), &b),
            RouteTarget::Swarm { .. }
        ));
    }

    #[test]
    fn agent_routing_unroutable_when_private_and_swarm_down() {
        let b = board_with("a", "code", 5, 0, 0.0);
        let p = RoutePolicy {
            privacy: Privacy::LocalOnly,
            swarm_available: false,
            ..Default::default()
        };
        assert!(matches!(
            route("code", &p, &b),
            RouteTarget::Unroutable { .. }
        ));
    }

    #[test]
    fn agent_routing_external_when_swarm_down_and_evidence() {
        let b = board_with("cursor", "code", 4, 0, 0.0);
        let p = RoutePolicy {
            swarm_available: false,
            ..Default::default()
        };
        assert!(matches!(
            route("code", &p, &b),
            RouteTarget::External { .. }
        ));
    }
}
