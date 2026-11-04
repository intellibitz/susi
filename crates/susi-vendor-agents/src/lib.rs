#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Third-party agent integrations — external task executors and framework
//! adapters (aider, autogen, crewai, openhands, e2b, gemini-cli, haystack,
//! temporal, …). Provider output is evidence of execution, never proof that
//! the requested change is correct. SUSI crates consume this surface through
//! `susi_agents::external` (re-export) or this crate directly.

pub use susi_config;
pub use susi_core;
pub use susi_error;
pub use susi_sandbox_client as susi_sandbox;

pub mod agent_scoreboard;
pub mod external;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use agent_scoreboard::{AgentRun, Scoreboard};

    #[test]
    fn scoreboard_new_starts_with_empty_scores() {
        let board = Scoreboard::new();
        assert!(board.scores().is_empty());
    }

    #[test]
    fn scoreboard_record_aggregates_attempts_and_success_rate() {
        let mut board = Scoreboard::new();
        board.record(AgentRun {
            agent: "aider".into(),
            task_class: "code".into(),
            success: true,
            duration_ms: 100,
            cost_usd: 0.02,
            unix_ms: 1,
        });
        board.record(AgentRun {
            agent: "aider".into(),
            task_class: "code".into(),
            success: false,
            duration_ms: 300,
            cost_usd: 0.02,
            unix_ms: 2,
        });
        let scores = board.scores();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].attempts, 2);
        assert_eq!(scores[0].successes, 1);
    }

    #[test]
    fn scoreboard_sorts_higher_success_rate_agent_ahead_of_lower() {
        let mut board = Scoreboard::new();
        board.record(AgentRun {
            agent: "low-agent".into(),
            task_class: "code".into(),
            success: false,
            duration_ms: 50,
            cost_usd: 0.0,
            unix_ms: 1,
        });
        board.record(AgentRun {
            agent: "top-agent".into(),
            task_class: "code".into(),
            success: true,
            duration_ms: 50,
            cost_usd: 0.01,
            unix_ms: 2,
        });
        let scores = board.scores();
        assert_eq!(scores[0].agent, "top-agent");
        assert_eq!(scores[1].agent, "low-agent");
    }

    #[test]
    fn scoreboard_cost_per_success_is_none_when_zero_runs_succeeded() {
        let mut board = Scoreboard::new();
        board.record(AgentRun {
            agent: "failing".into(),
            task_class: "code".into(),
            success: false,
            duration_ms: 100,
            cost_usd: 0.05,
            unix_ms: 1,
        });
        let scores = board.scores();
        assert!(scores[0].cost_per_success.is_none());
    }
}
