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
/// Turns one worker attempt may take. Large roadmap tasks read a contract
/// and a 300 KB test file before writing; 25 turns reached the write turn and
/// stopped, so the cap leaves room to read, write and iterate while the budget
/// and the attempt ladder still bound the cost.
pub const MAX_TURNS: usize = 60;

/// One headless agent turn.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Gateway base URL, e.g. `http://127.0.0.1:9185`.
    pub gateway_url: String,
    /// Model id as the gateway's catalogue knows it, e.g. `deepseek-flash`.
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

impl ExecutorConfig {
    /// Verify structural invariants of an [`ExecutorConfig`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `gateway_url` lacks an HTTP(S) scheme,
    /// `model`, `mission`, `task`, or `prompt` is blank, `workspace` or
    /// `state_dir` is empty, `attempt` is zero, or `timeout` is zero.
    pub fn verify_config_invariants(&self) -> Result<(), String> {
        let url = self.gateway_url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(
                "ExecutorConfig.gateway_url must start with http:// or https://".to_string(),
            );
        }
        if self.model.trim().is_empty() {
            return Err("ExecutorConfig.model must not be empty".to_string());
        }
        if self.workspace.as_os_str().is_empty() {
            return Err("ExecutorConfig.workspace must not be empty".to_string());
        }
        if self.state_dir.as_os_str().is_empty() {
            return Err("ExecutorConfig.state_dir must not be empty".to_string());
        }
        if self.mission.trim().is_empty() {
            return Err("ExecutorConfig.mission must not be empty".to_string());
        }
        if self.task.trim().is_empty() {
            return Err("ExecutorConfig.task must not be empty".to_string());
        }
        if self.attempt == 0 {
            return Err("ExecutorConfig.attempt must be at least 1".to_string());
        }
        if self.timeout.is_zero() {
            return Err("ExecutorConfig.timeout must be greater than zero".to_string());
        }
        if self.prompt.trim().is_empty() {
            return Err("ExecutorConfig.prompt must not be empty".to_string());
        }
        Ok(())
    }
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

impl RunOutcome {
    /// Verify structural invariants of a [`RunOutcome`].
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `Succeeded` has blank `final_text` or
    /// a non-finite/negative `reported_cost_usd`, or if `Failed` has a blank
    /// `message`.
    pub fn verify_outcome_invariants(&self) -> Result<(), String> {
        match self {
            Self::Succeeded {
                final_text,
                reported_cost_usd,
            } => {
                if final_text.trim().is_empty() {
                    return Err("RunOutcome::Succeeded.final_text must not be empty".to_string());
                }
                if let Some(cost) = reported_cost_usd {
                    if !cost.is_finite() || *cost < 0.0 {
                        return Err(
                            "RunOutcome::Succeeded.reported_cost_usd must be finite and non-negative"
                                .to_string(),
                        );
                    }
                }
                Ok(())
            }
            Self::Failed { message } => {
                if message.trim().is_empty() {
                    return Err("RunOutcome::Failed.message must not be empty".to_string());
                }
                Ok(())
            }
            Self::TimedOut => Ok(()),
        }
    }
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
            model: "deepseek-flash".into(),
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
        assert_eq!(MAX_TURNS, 60);
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

    #[test]
    fn executor_config_and_run_outcome_variants_preserve_clone_debug_and_partial_eq_contracts() {
        let cfg = config();
        let cloned = cfg.clone();
        assert_eq!(cloned.gateway_url, cfg.gateway_url);
        assert_eq!(cloned.model, cfg.model);
        assert_eq!(cloned.workspace, cfg.workspace);
        assert_eq!(cloned.state_dir, cfg.state_dir);
        assert_eq!(cloned.mission, cfg.mission);
        assert_eq!(cloned.task, cfg.task);
        assert_eq!(cloned.attempt, cfg.attempt);
        assert_eq!(cloned.timeout, cfg.timeout);
        assert_eq!(cloned.prompt, cfg.prompt);
        assert!(!format!("{cloned:?}").is_empty());

        let succeeded = RunOutcome::Succeeded {
            final_text: "ok".into(),
            reported_cost_usd: Some(0.05),
        };
        let failed = RunOutcome::Failed {
            message: "err".into(),
        };
        let timed_out = RunOutcome::TimedOut;
        assert_eq!(succeeded.clone(), succeeded);
        assert_eq!(failed.clone(), failed);
        assert_eq!(timed_out.clone(), timed_out);
        assert_ne!(succeeded, failed);
        assert_ne!(failed, timed_out);
        assert_ne!(succeeded, timed_out);
    }

    #[test]
    fn run_delegates_to_worker_and_succeeded_without_reported_cost_compares_equal() {
        let unpriced = RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        };
        assert_eq!(unpriced.clone(), unpriced);

        let mut cfg = config();
        cfg.state_dir =
            std::env::temp_dir().join(format!("susi-worker-direct-{}", std::process::id()));
        cfg.gateway_url = "http://127.0.0.1:1".into();
        let outcome = run(&cfg).expect("closed port returns RunOutcome::Failed");
        assert!(matches!(outcome, RunOutcome::Failed { .. }));
    }

    #[test]
    fn executor_config_and_outcome_verify_invariants_reject_corrupt_state() {
        let valid_cfg = config();
        assert!(valid_cfg.verify_config_invariants().is_ok());

        let bad_url = ExecutorConfig {
            gateway_url: "ws://127.0.0.1:9185".into(),
            ..config()
        };
        assert!(bad_url.verify_config_invariants().is_err());

        let blank_model = ExecutorConfig {
            model: " ".into(),
            ..config()
        };
        assert!(blank_model.verify_config_invariants().is_err());

        let empty_workspace = ExecutorConfig {
            workspace: PathBuf::new(),
            ..config()
        };
        assert!(empty_workspace.verify_config_invariants().is_err());

        let empty_state_dir = ExecutorConfig {
            state_dir: PathBuf::new(),
            ..config()
        };
        assert!(empty_state_dir.verify_config_invariants().is_err());

        let blank_mission = ExecutorConfig {
            mission: String::new(),
            ..config()
        };
        assert!(blank_mission.verify_config_invariants().is_err());

        let blank_task = ExecutorConfig {
            task: "   ".into(),
            ..config()
        };
        assert!(blank_task.verify_config_invariants().is_err());

        let zero_attempt = ExecutorConfig {
            attempt: 0,
            ..config()
        };
        assert!(zero_attempt.verify_config_invariants().is_err());

        let zero_timeout = ExecutorConfig {
            timeout: Duration::ZERO,
            ..config()
        };
        assert!(zero_timeout.verify_config_invariants().is_err());

        let blank_prompt = ExecutorConfig {
            prompt: " \t ".into(),
            ..config()
        };
        assert!(blank_prompt.verify_config_invariants().is_err());

        let ok_succeeded = RunOutcome::Succeeded {
            final_text: "completed".into(),
            reported_cost_usd: Some(0.12),
        };
        assert!(ok_succeeded.verify_outcome_invariants().is_ok());

        let ok_unpriced = RunOutcome::Succeeded {
            final_text: "completed".into(),
            reported_cost_usd: None,
        };
        assert!(ok_unpriced.verify_outcome_invariants().is_ok());

        let blank_final = RunOutcome::Succeeded {
            final_text: "   ".into(),
            reported_cost_usd: None,
        };
        assert!(blank_final.verify_outcome_invariants().is_err());

        let neg_cost = RunOutcome::Succeeded {
            final_text: "completed".into(),
            reported_cost_usd: Some(-0.01),
        };
        assert!(neg_cost.verify_outcome_invariants().is_err());

        let nan_cost = RunOutcome::Succeeded {
            final_text: "completed".into(),
            reported_cost_usd: Some(f64::NAN),
        };
        assert!(nan_cost.verify_outcome_invariants().is_err());

        let ok_failed = RunOutcome::Failed {
            message: "gateway refused".into(),
        };
        assert!(ok_failed.verify_outcome_invariants().is_ok());

        let blank_failed = RunOutcome::Failed {
            message: String::new(),
        };
        assert!(blank_failed.verify_outcome_invariants().is_err());

        assert!(RunOutcome::TimedOut.verify_outcome_invariants().is_ok());
    }
}
