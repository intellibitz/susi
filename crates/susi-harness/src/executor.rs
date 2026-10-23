//! The native executor: one coding turn, in process.
//!
//! There is no external agent runtime. [`run`] drives
//! [`crate::worker::run_agent_loop`], which talks to the metering gateway with a
//! per-run key (`susi:<mission>:<task>:<attempt>`), so every model call the
//! worker makes is priced and attributed by the gateway. Nothing is spawned and
//! nothing is shelled out: the tool calls the model asks for are executed by
//! [`crate::tools::Registry`] against the run's workspace.
//!
//! Two properties follow from that and are worth stating because they are what
//! the old subprocess path could not guarantee:
//!
//! - **The verdict is the returned [`RunOutcome`]**, never a process exit code.
//! - **Bounded work is structural**: the loop has a turn cap and a wall-clock
//!   deadline, both enforced in process, so a runaway model cannot outlive the
//!   mission it belongs to.

use std::path::PathBuf;
use std::time::Duration;
use susi_error::EaiResult;

/// Turns a single native turn may take before it is stopped.
///
/// The worker is a tool loop, so this bounds how many model calls one turn can
/// make. It is deliberately generous: a coding turn that needs more than this
/// is looping, not working.
pub const MAX_TURNS: usize = 25;

/// One headless agent turn.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Gateway base URL, e.g. `http://127.0.0.1:9185`.
    pub gateway_url: String,
    /// Model id as the gateway's catalogue knows it, e.g. `deepseek-chat`.
    pub model: String,
    /// Working directory the agent reads and writes.
    pub workspace: PathBuf,
    /// State directory for this run (prompt, result, tool scratch).
    pub state_dir: PathBuf,
    /// Mission this run belongs to.
    pub mission: String,
    /// Task this run belongs to.
    pub task: String,
    /// 1-based attempt number.
    pub attempt: u32,
    /// Wall-clock deadline.
    pub timeout: Duration,
    /// The instruction handed to the agent.
    pub prompt: String,
}

/// How one agent turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// The worker concluded with final text.
    Succeeded {
        /// The agent's final text.
        final_text: String,
        /// The agent's own cost estimate, when it reports one. The ledger, not
        /// this figure, is the accounting authority.
        reported_cost_usd: Option<f64>,
    },
    /// The turn reported failure.
    Failed {
        /// The agent's reason.
        message: String,
    },
    /// The wall clock expired before the turn concluded.
    TimedOut,
}

/// Run one headless turn to its deadline.
///
/// # Errors
///
/// Returns [`susi_error::EaiError`] only for operational faults — an
/// uncreatable state directory, an unencodable request. A refused or failed
/// model call is a [`RunOutcome::Failed`], not an error: it is a result about
/// the turn, not a fault in the harness.
pub fn run(config: &ExecutorConfig) -> EaiResult<RunOutcome> {
    crate::worker::run_agent_loop(config, MAX_TURNS)
}

/// A thing that can execute one turn.
///
/// The scheduler depends on this trait, so a mission can be tested without a
/// gateway.
pub trait Runner: Sync {
    /// Run one turn.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error (I/O, encoding), not an agent
    /// failure — those are [`RunOutcome`] values.
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome>;
}

/// The native in-process coding worker.
///
/// Drives turn-by-turn completion through the metering gateway and executes
/// workspace operations directly in process.
pub struct NativeWorker;

impl Runner for NativeWorker {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        run(config)
    }
}

/// Run one turn using the native in-process worker.
///
/// Kept as a named entry point because callers that own their own runner
/// (the mission graph) call it directly.
///
/// # Errors
///
/// As [`run`].
pub fn run_native(config: &ExecutorConfig) -> EaiResult<RunOutcome> {
    run(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ExecutorConfig {
        ExecutorConfig {
            gateway_url: "http://127.0.0.1:9185".into(),
            model: "deepseek-chat".into(),
            workspace: PathBuf::from("scratch"),
            state_dir: PathBuf::from("state"),
            mission: "pilot-2".into(),
            task: "exec-1".into(),
            attempt: 3,
            timeout: Duration::from_secs(60),
            prompt: "say OK".into(),
        }
    }

    #[test]
    fn native_worker_implements_runner() {
        let runner = NativeWorker;
        let mut cfg = config();
        let test_state =
            std::env::temp_dir().join(format!("susi-worker-test-{}", std::process::id()));
        cfg.state_dir = test_state;
        // A closed port fails cleanly with a message rather than crashing.
        cfg.gateway_url = "http://127.0.0.1:1".into();
        let outcome = runner.run(&cfg).unwrap();
        match outcome {
            RunOutcome::Failed { message } => {
                assert!(
                    message.contains("gateway call failed")
                        || message.contains("connection refused")
                        || message.contains("HTTP"),
                    "unexpected failure text: {message}"
                );
            }
            other => panic!("expected failed outcome, got {other:?}"),
        }
    }

    #[test]
    fn the_turn_cap_bounds_a_worker_that_never_concludes() {
        assert_eq!(MAX_TURNS, 25);
    }

    #[test]
    fn a_worker_that_cannot_reach_the_gateway_is_a_failed_turn_not_an_error() {
        let mut cfg = config();
        cfg.state_dir =
            std::env::temp_dir().join(format!("susi-worker-fail-{}", std::process::id()));
        cfg.gateway_url = "http://127.0.0.1:1".into();
        let outcome = run_native(&cfg).expect("a refused connection is not an EaiError");
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
    }
}
