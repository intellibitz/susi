//! The supervisor loop.
//!
//! A plan lists the tasks of one mission. The supervisor runs each task
//! through a [`TaskExecutor`], persists the result after every task, and on a
//! rerun skips what is already done — so a crashed or interrupted mission
//! resumes instead of restarting.
//!
//! The susi executor ([`SusiExecutor`]) is the production implementation: it
//! prepares a worktree, claims the task, runs the agent, verifies, and lands
//! it. Tests use a fake executor, so the loop is exercised without a repo.

use crate::adapter::{
    land, run_task, CommandRunner, SusiRunPolicy, SusiRunReport, SusiTarget, SusiTaskSpec,
};
use crate::adaptive::{
    self, AdaptiveRunner, CliSubRunner, CliToolKind, SusiSocketEndpoints, GAWD_A2A_PORT,
    GEMI_OPENAI_PORT, GMCP_HTTP_PORT,
};
use crate::budget::{Budget, BudgetGuard};
use crate::catalog::PriceCatalogue;
use crate::control::ControlState;
use crate::executor::{ExecutorConfig, NativeWorker, RunOutcome, Runner};
use crate::lifecycle::{self, LifecycleEvent};
use crate::policy::Policy;
use crate::workflow;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

/// Alias for [`CliToolKind`] representing a monthly-subscription CLI agent backend.
pub type CliAgentBackend = CliToolKind;

/// Status of one monthly-subscription CLI backend (`claude`, `codex`, `gemini`, `agy`, `qwen`, `aider`, `opencode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliToolStatus {
    /// Canonical tool identifier (e.g. `"claude"`, `"codex"`).
    pub kind: String,
    /// True when an executable for this tool was found in the search directories.
    pub installed: bool,
    /// Discovered executable path, when installed.
    pub executable_path: Option<PathBuf>,
    /// Candidate binary names searched for this tool.
    pub candidates: Vec<String>,
}

/// Active adaptive execution policy (strategy and failover models).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveExecutionPolicy {
    /// Strategy name (`"auto"`, `"cli-first"`, `"payg-first"`, `"payg-only"`, `"cli-only"`).
    pub strategy: String,
    /// Optional CLI spec PAYG fails over to (e.g. `"cli:claude"`).
    pub fallback_cli_model: Option<String>,
    /// Optional PAYG model a CLI turn fails over to (e.g. `"deepseek-flash"`).
    pub fallback_payg_model: Option<String>,
}

impl Default for AdaptiveExecutionPolicy {
    fn default() -> Self {
        Self::from_runner(&AdaptiveRunner::default())
    }
}

impl AdaptiveExecutionPolicy {
    /// Snapshot the policy configured on `runner`.
    #[must_use]
    pub fn from_runner<P: Runner, C: Runner>(runner: &AdaptiveRunner<P, C>) -> Self {
        Self {
            strategy: runner.strategy.as_str().to_string(),
            fallback_cli_model: runner.fallback_cli_model.clone(),
            fallback_payg_model: runner.fallback_payg_model.clone(),
        }
    }

    /// Snapshot the policy from environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_runner(&AdaptiveRunner::from_env())
    }
}

/// Serializable Susi AI OS Socket endpoints (`9090` GMCP, `9091` GEMI, `9094` GAWD).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SusiSocketReport {
    /// GMCP streamable-HTTP tool socket URL.
    pub gmcp_http_url: String,
    /// Gemi OpenAI-compatible chat-completions socket URL.
    pub gemi_openai_url: String,
    /// Gawd agent-to-agent socket URL.
    pub gawd_a2a_url: String,
    /// GMCP HTTP port (`9090 + offset`).
    pub gmcp_port: u16,
    /// Gemi OpenAI port (`9091 + offset`).
    pub gemi_port: u16,
    /// Gawd A2A port (`9094 + offset`).
    pub gawd_port: u16,
}

impl SusiSocketReport {
    /// Build a socket report for `offset`.
    #[must_use]
    pub fn from_port_offset(offset: u16) -> Self {
        let endpoints = SusiSocketEndpoints::from_port_offset(offset);
        Self {
            gmcp_http_url: endpoints.gmcp_http_url,
            gemi_openai_url: endpoints.gemi_openai_url,
            gawd_a2a_url: endpoints.gawd_a2a_url,
            gmcp_port: GMCP_HTTP_PORT.saturating_add(offset),
            gemi_port: GEMI_OPENAI_PORT.saturating_add(offset),
            gawd_port: GAWD_A2A_PORT.saturating_add(offset),
        }
    }
}

impl Default for SusiSocketReport {
    fn default() -> Self {
        Self::from_port_offset(0)
    }
}

/// Request to execute a single task turn through the adaptive CLI + PAYG BYOK runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveTaskRequest {
    /// Mission identifier.
    pub mission: String,
    /// Task key.
    pub task: String,
    /// Model or `cli:<tool>` spec.
    pub model: String,
    /// Task prompt piped to the worker.
    pub prompt: String,
    /// Workspace directory.
    pub workspace: PathBuf,
    /// Per-task state directory.
    pub state_dir: PathBuf,
    /// Metering gateway base URL.
    pub gateway_url: String,
    /// Turn timeout in seconds.
    pub timeout_secs: u64,
}

impl AdaptiveTaskRequest {
    /// Convert into an [`ExecutorConfig`].
    #[must_use]
    pub fn to_executor_config(&self) -> ExecutorConfig {
        ExecutorConfig {
            gateway_url: self.gateway_url.clone(),
            model: self.model.clone(),
            workspace: self.workspace.clone(),
            state_dir: self.state_dir.clone(),
            mission: self.mission.clone(),
            task: self.task.clone(),
            attempt: 1,
            timeout: Duration::from_secs(self.timeout_secs.max(1)),
            prompt: self.prompt.clone(),
        }
    }
}

/// Outcome of an [`AdaptiveTaskRequest`] executed by [`AdaptiveRunner`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdaptiveTaskOutcome {
    /// Primary billing mode (`"payg-byok"` or `"monthly-sub-cli"`).
    pub primary_mode: String,
    /// Primary model or CLI spec.
    pub primary_model: String,
    /// Failover billing mode, if configured.
    pub failover_mode: Option<String>,
    /// Failover model or CLI spec, if configured.
    pub failover_model: Option<String>,
    /// True when the turn succeeded on either primary or failover.
    pub succeeded: bool,
    /// Human-readable status description.
    pub status: String,
    /// Final text returned by the worker on success.
    pub final_text: Option<String>,
    /// Reported PAYG cost in USD (`Some(0.0)` for subscription CLIs).
    pub reported_cost_usd: Option<f64>,
}

/// One node in an embedded workflow wave plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNode {
    /// Unique task key within the wave plan.
    pub key: String,
    /// Task keys that must complete before this node starts.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Repository-relative file or directory scopes reserved by this node.
    pub scopes: Vec<String>,
}

impl workflow::WorkflowTask for WorkflowNode {
    fn key(&self) -> &str {
        &self.key
    }

    fn deps(&self) -> &[String] {
        &self.deps
    }

    fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

/// Wave schedule produced by [`EmbeddedWorkflowOrchestrator`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowWavePlan {
    /// Identifier of this wave plan.
    pub wave_id: String,
    /// Ordered waves; each wave lists task keys that can run concurrently without scope or dependency conflicts.
    pub waves: Vec<Vec<String>>,
    /// Task keys blocked by external active claims.
    pub blocked: Vec<String>,
}

/// Error returned when planning an embedded workflow wave fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEmbedError {
    /// Reason the workflow plan was rejected.
    pub message: String,
}

impl std::fmt::Display for WorkflowEmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowEmbedError {}

/// In-process orchestrator for dependency- and scope-disjoint wave planning.
#[derive(Debug, Clone)]
pub struct EmbeddedWorkflowOrchestrator {
    /// Maximum concurrent tasks per wave.
    pub max_parallel: usize,
    /// Already-completed task keys.
    pub completed: BTreeSet<String>,
    /// Scopes currently claimed by external agents.
    pub active_claimed_scopes: Vec<String>,
}

impl Default for EmbeddedWorkflowOrchestrator {
    fn default() -> Self {
        Self {
            max_parallel: 4,
            completed: BTreeSet::new(),
            active_claimed_scopes: Vec::new(),
        }
    }
}

impl EmbeddedWorkflowOrchestrator {
    /// Create an orchestrator with `max_parallel` concurrency.
    #[must_use]
    pub fn new(max_parallel: usize) -> Self {
        Self {
            max_parallel: max_parallel.max(1),
            ..Self::default()
        }
    }

    /// Partition `nodes` into dependency-ready, pairwise scope-disjoint waves.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowEmbedError`] if any scope is invalid or if dependencies contain an unknown key or cycle.
    pub fn plan_wave(
        &self,
        wave_id: &str,
        nodes: &[WorkflowNode],
    ) -> Result<WorkflowWavePlan, WorkflowEmbedError> {
        if wave_id.trim().is_empty() {
            return Err(WorkflowEmbedError {
                message: "wave_id must not be empty".to_string(),
            });
        }
        for node in nodes {
            if node.scopes.is_empty() {
                return Err(WorkflowEmbedError {
                    message: format!("task {} reserves no scopes", node.key),
                });
            }
            for scope in &node.scopes {
                workflow::normalize_scope(scope).map_err(|error| WorkflowEmbedError {
                    message: format!("task {} scope: {error}", node.key),
                })?;
            }
        }
        let schedule = workflow::WorkflowSchedule::build(
            nodes,
            &self.completed,
            &self.active_claimed_scopes,
            self.max_parallel.max(1),
        )
        .map_err(|error| WorkflowEmbedError {
            message: error.to_string(),
        })?;
        Ok(WorkflowWavePlan {
            wave_id: wave_id.to_string(),
            waves: schedule
                .waves
                .into_iter()
                .map(|w| w.tasks.into_iter().map(|t| t.key.clone()).collect())
                .collect(),
            blocked: schedule
                .blocked
                .into_iter()
                .map(|t| t.key.clone())
                .collect(),
        })
    }
}

/// Complete report of discovered CLI subscription tools, Susi AI OS Socket endpoints,
/// active adaptive execution policy, embedded workflow readiness, and Mandate 73
/// recursive container contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveSupervisorReport {
    /// Status of all 7 supported monthly-subscription CLI tools.
    pub cli_fleet: Vec<CliToolStatus>,
    /// Susi AI OS Socket endpoint bindings (`9090` GMCP, `9091` GEMI, `9094` GAWD).
    pub socket_endpoints: SusiSocketReport,
    /// Active adaptive execution policy.
    pub policy: AdaptiveExecutionPolicy,
    /// True when the embedded workflow engine is ready in-process.
    pub embedded_workflow_ready: bool,
    /// Verified Driver/Engine container contracts discovered on this host.
    #[serde(default)]
    pub recursive_contracts: Vec<crate::recursive::CapabilityContract>,
}

/// Discover the status of all 7 supported monthly-subscription CLIs in `dirs`.
#[must_use]
pub fn discover_cli_fleet_in(dirs: &[PathBuf]) -> Vec<CliToolStatus> {
    let found = adaptive::discover_cli_tools_in(dirs);
    CliToolKind::ALL
        .iter()
        .map(|kind| {
            let path = found
                .iter()
                .find(|entry| entry.kind == *kind)
                .map(|entry| entry.path.clone());
            CliToolStatus {
                kind: kind.as_str().to_string(),
                installed: path.is_some(),
                executable_path: path,
                candidates: kind.candidates().into_iter().map(str::to_string).collect(),
            }
        })
        .collect()
}

/// Discover the status of all 7 supported monthly-subscription CLIs in [`adaptive::default_search_dirs`].
#[must_use]
pub fn discover_cli_fleet() -> Vec<CliToolStatus> {
    discover_cli_fleet_in(&adaptive::default_search_dirs())
}

/// Build Mandate 73 [`crate::recursive::CapabilityContract`] descriptors for the
/// in-process `susi-harness` container and all CLI backends discovered in `dirs`.
#[must_use]
pub fn discover_verified_container_contracts_in(
    dirs: &[PathBuf],
) -> Vec<crate::recursive::CapabilityContract> {
    use crate::recursive::{CapabilityContract, ContainerRole, RECURSIVE_CONTRACT_SCHEMA};

    let mut contracts = vec![CapabilityContract {
        contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
        component_id: "susi-harness".to_string(),
        role: ContainerRole::DriverAndEngine,
        capabilities: ["code_edit", "distill", "review", "supervise", "wave_plan"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        verified_available: true,
    }];
    for tool in discover_cli_fleet_in(dirs) {
        contracts.push(CapabilityContract {
            contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
            component_id: format!("cli:{}", tool.kind),
            role: ContainerRole::DriverAndEngine,
            capabilities: ["code_edit", "review", "wave_plan"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            verified_available: tool.installed,
        });
    }
    contracts
}

/// Discover Mandate 73 [`crate::recursive::CapabilityContract`] descriptors in [`adaptive::default_search_dirs`].
#[must_use]
pub fn discover_verified_container_contracts() -> Vec<crate::recursive::CapabilityContract> {
    discover_verified_container_contracts_in(&adaptive::default_search_dirs())
}

struct SharedRunner(std::sync::Arc<dyn Runner + Send + Sync>);

impl Runner for SharedRunner {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        self.0.run(config)
    }
}

/// Parameters for executing a Mandate 73 [`crate::recursive::RecursiveTaskNode`] tree.
pub struct RecursiveTreeRun<'a> {
    /// Root directory under which isolated per-worker sandboxes are allocated.
    pub sandbox_root: &'a Path,
    /// Metering gateway base URL.
    pub gateway_url: &'a str,
    /// Default model id used for non-CLI container components.
    pub default_model: &'a str,
    /// Root run identifier (`"mission-1"`).
    pub root_run_id: &'a str,
    /// Root task node (leaf Engine or nested Driver wave tree).
    pub node: &'a crate::recursive::RecursiveTaskNode,
    /// Base port offset for disjoint 5-port Susi sockets.
    pub base_port_offset: u16,
    /// Cooperative cancellation token linked to the caller's scope.
    pub cancel: &'a crate::recursive::CancellationToken,
}

/// Execute `run` using `runner` and `contracts` inside isolated per-worker sandboxes.
pub fn execute_recursive_tree_with<R: Runner + Send + Sync + 'static>(
    run: &RecursiveTreeRun<'_>,
    runner: R,
    contracts: &[crate::recursive::CapabilityContract],
) -> Result<crate::recursive::ExecutionProvenanceReceipt, crate::recursive::RecursiveError> {
    use crate::recursive::{RecursiveRuntime, RunnerContainerBridge, MIN_ISOLATED_PORT_STRIDE};

    let shared: std::sync::Arc<dyn Runner + Send + Sync> = std::sync::Arc::new(runner);
    let mut runtime = RecursiveRuntime::new(run.sandbox_root, MIN_ISOLATED_PORT_STRIDE)?;
    for contract in contracts {
        let model = if contract.component_id.starts_with("cli:") {
            contract.component_id.clone()
        } else {
            run.default_model.to_string()
        };
        let bridge = RunnerContainerBridge::new(
            contract.clone(),
            SharedRunner(std::sync::Arc::clone(&shared)),
            run.gateway_url.to_string(),
            model,
        );
        runtime.register(std::sync::Arc::new(bridge))?;
    }
    runtime.execute(run.root_run_id, run.node, run.base_port_offset, run.cancel)
}

/// Execute `run` through the environment-configured [`AdaptiveRunner`] and live
/// verified container contracts discovered on this host.
pub fn execute_recursive_tree(
    run: &RecursiveTreeRun<'_>,
) -> Result<crate::recursive::ExecutionProvenanceReceipt, crate::recursive::RecursiveError> {
    let contracts = discover_verified_container_contracts();
    execute_recursive_tree_with(run, AdaptiveRunner::from_env(), &contracts)
}

/// Build an [`AdaptiveSupervisorReport`] for `port_offset`.
#[must_use]
pub fn build_adaptive_supervisor_report(port_offset: u16) -> AdaptiveSupervisorReport {
    let search_dirs = adaptive::default_search_dirs();
    AdaptiveSupervisorReport {
        cli_fleet: discover_cli_fleet_in(&search_dirs),
        socket_endpoints: SusiSocketReport::from_port_offset(port_offset),
        policy: AdaptiveExecutionPolicy::from_env(),
        embedded_workflow_ready: true,
        recursive_contracts: discover_verified_container_contracts_in(&search_dirs),
    }
}

/// Execute `request` with `runner` and return a structured [`AdaptiveTaskOutcome`].
#[must_use]
pub fn execute_adaptive_task_with<P: Runner, C: Runner>(
    runner: &AdaptiveRunner<P, C>,
    request: &AdaptiveTaskRequest,
) -> AdaptiveTaskOutcome {
    let route = runner.plan(&request.model);
    let config = request.to_executor_config();
    let (primary_mode, primary_model) = (route.primary.0.as_str().to_string(), route.primary.1);
    let (failover_mode, failover_model) = match route.failover {
        Some((mode, model)) => (Some(mode.as_str().to_string()), Some(model)),
        None => (None, None),
    };
    match runner.run(&config) {
        Ok(RunOutcome::Succeeded {
            final_text,
            reported_cost_usd,
        }) => AdaptiveTaskOutcome {
            primary_mode,
            primary_model,
            failover_mode,
            failover_model,
            succeeded: true,
            status: "succeeded".to_string(),
            final_text: Some(final_text),
            reported_cost_usd,
        },
        Ok(RunOutcome::Failed { message }) => AdaptiveTaskOutcome {
            primary_mode,
            primary_model,
            failover_mode,
            failover_model,
            succeeded: false,
            status: format!("failed: {message}"),
            final_text: None,
            reported_cost_usd: None,
        },
        Ok(RunOutcome::TimedOut) => AdaptiveTaskOutcome {
            primary_mode,
            primary_model,
            failover_mode,
            failover_model,
            succeeded: false,
            status: "timed_out".to_string(),
            final_text: None,
            reported_cost_usd: None,
        },
        Err(error) => AdaptiveTaskOutcome {
            primary_mode,
            primary_model,
            failover_mode,
            failover_model,
            succeeded: false,
            status: format!("error: {error}"),
            final_text: None,
            reported_cost_usd: None,
        },
    }
}

/// One task in a supervisor plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanTask {
    /// Stable key within the plan (also the worktree suffix and state key).
    pub key: String,
    /// Task title handed to `susi tasks add` (or the queue task's title).
    pub title: String,
    /// The task's acceptance command.
    pub accept: String,
    /// Paths the task reserves.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Worker model for this task. When absent, the executor's model is used.
    ///
    /// One mission can therefore run a DeepSeek worker on one task and a
    /// Gemini worker on the next.
    #[serde(default)]
    pub model: Option<String>,
    /// An existing queue task to claim instead of minting a new one.
    ///
    /// When set, the worktree already carries the task record, the adapter
    /// claims that id, and the acceptance comes from the record.
    #[serde(default)]
    pub task: Option<String>,
    /// Keys of tasks in this plan that must land before this one starts.
    ///
    /// A task whose dependency fails this run fails too, unattempted.
    #[serde(default)]
    pub deps: Vec<String>,
    /// The instruction for the agent.
    pub prompt: String,
}

/// A mission plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Plan {
    /// Mission id.
    pub mission: String,
    /// Tasks, in run order.
    pub tasks: Vec<PlanTask>,
}

impl Plan {
    /// Parse and validate a plan document.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` for malformed JSON, an empty plan, a blank
    /// field, a missing scope, or a duplicate key.
    pub fn from_json(text: &str) -> EaiResult<Self> {
        let plan: Self = serde_json::from_str(text)
            .map_err(|error| EaiError::config(format!("plan: {error}")))?;
        plan.validate()?;
        Ok(plan)
    }

    /// Read and validate a plan file.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the file cannot be read, or the same errors
    /// as [`Plan::from_json`].
    pub fn load(path: &Path) -> EaiResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| EaiError::io(format!("read plan {}: {error}", path.display())))?;
        Self::from_json(&text)
    }

    /// Validate the plan's shape: a mission, at least one task, unique keys,
    /// no empty acceptance, prompt or scope, every scope normalizable, and
    /// dependencies that name tasks in the plan without forming a cycle.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` naming the first violation.
    pub fn validate(&self) -> EaiResult<()> {
        if self.mission.trim().is_empty() {
            return Err(EaiError::config("plan mission is required"));
        }
        if self.tasks.is_empty() {
            return Err(EaiError::config("plan has no tasks"));
        }
        let mut keys = BTreeSet::new();
        for task in &self.tasks {
            if task.key.trim().is_empty() {
                return Err(EaiError::config("plan task key is required"));
            }
            if task.title.trim().is_empty() {
                return Err(EaiError::config(format!("task {} has no title", task.key)));
            }
            if task.accept.trim().is_empty() {
                return Err(EaiError::config(format!(
                    "task {} has no acceptance",
                    task.key
                )));
            }
            if task.prompt.trim().is_empty() {
                return Err(EaiError::config(format!("task {} has no prompt", task.key)));
            }
            if task.scopes.is_empty() {
                return Err(EaiError::config(format!(
                    "task {} reserves no scopes",
                    task.key
                )));
            }
            for scope in &task.scopes {
                workflow::normalize_scope(scope).map_err(|error| {
                    EaiError::config(format!("task {} scope: {error}", task.key))
                })?;
            }
            if !keys.insert(task.key.clone()) {
                return Err(EaiError::config(format!(
                    "task key {} appears twice",
                    task.key
                )));
            }
        }
        workflow::validate_dependencies(&self.tasks)
    }
}

/// How one task ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// The task was prepared, accepted and landed.
    Done {
        /// The task id the target minted.
        task: String,
    },
    /// The task did not land.
    Failed {
        /// Why.
        reason: String,
    },
}

/// What the supervisor did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisorReport {
    /// Mission id.
    pub mission: String,
    /// Task keys that landed this run.
    pub done: Vec<String>,
    /// Task keys that failed this run, with the reason.
    pub failed: Vec<(String, String)>,
    /// Task keys skipped because an earlier run landed them.
    pub skipped: Vec<String>,
    /// Task keys not started because the mission budget was exhausted, or
    /// because no model on a task's ladder can be paid for (every model's
    /// last real call was refused: the key was refused or has no credit).
    /// A task prepared in the batch that stopped is here unless its work
    /// passed and landed.
    pub unstarted: Vec<String>,
    /// True when the run stopped early for a top-up: the mission budget was
    /// exhausted, or no model on a task's ladder can be paid for (every
    /// model's last real call was refused). The rest of the plan is left for
    /// a later run rather than failed unattempted.
    pub budget_exhausted: bool,
    /// True when the operator paused the mission.
    pub paused: bool,
    /// True when the operator aborted the mission.
    pub aborted: bool,
}

/// Prepares and lands a planned task.
///
/// Preparation is the expensive, parallel half (worktree, claim, agent,
/// verification); landing is the serial half (close, push, merge) and runs on
/// the supervisor's thread.
pub trait TaskExecutor: Sync {
    /// What a prepared task carries to the landing step.
    type Prepared: Send;

    /// Prepare one task: everything up to and including verification.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error; a task that simply did not
    /// pass is still `Ok`.
    fn prepare(&self, task: &PlanTask) -> EaiResult<Self::Prepared>;

    /// Land a prepared task and report its final status.
    ///
    /// # Errors
    ///
    /// Returns the executor's operational error; a task that simply did not
    /// land is [`TaskStatus::Failed`].
    fn land(&self, prepared: Self::Prepared) -> EaiResult<TaskStatus>;

    /// True once the mission budget refused an attempt, or once no model on a
    /// task's ladder could be paid for (every model's last real call was
    /// refused: the key was refused or has no credit).
    ///
    /// The supervisor checks this after every task and stops the plan for a
    /// top-up instead of starting work the mission cannot pay for; work the
    /// stopped batch already prepared and that passed still lands.
    fn budget_exhausted(&self) -> bool {
        false
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    records: BTreeMap<String, Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    status: String,
    task: Option<String>,
    reason: Option<String>,
}

#[derive(Default)]
struct Tally {
    done: Vec<String>,
    failed: Vec<(String, String)>,
}

/// Runs a plan, persisting after every task.
pub struct Supervisor<'a, E: TaskExecutor> {
    /// Where the resume state lives.
    pub state_path: PathBuf,
    /// How tasks are executed.
    pub executor: &'a E,
    /// How many tasks prepare at once (landing is always serial).
    pub max_parallel: usize,
    /// Operator control log, when the cockpit can pause or abort the mission.
    pub control_path: Option<PathBuf>,
}

impl<'a, E: TaskExecutor> Supervisor<'a, E> {
    /// Build a supervisor that prepares one task at a time.
    #[must_use]
    pub fn new(state_path: impl Into<PathBuf>, executor: &'a E) -> Self {
        Self {
            state_path: state_path.into(),
            executor,
            max_parallel: 1,
            control_path: None,
        }
    }

    /// Set how many tasks prepare at once.
    #[must_use]
    pub fn with_parallelism(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel.max(1);
        self
    }

    /// Read operator commands from this log before each batch.
    #[must_use]
    pub fn with_controls(mut self, path: impl Into<PathBuf>) -> Self {
        self.control_path = Some(path.into());
        self
    }

    /// Build the default [`AdaptiveRunner`] configured from the environment.
    #[must_use]
    pub fn adaptive_runner(&self) -> AdaptiveRunner<NativeWorker, CliSubRunner> {
        AdaptiveRunner::from_env()
    }

    /// Discover the status of all 7 supported monthly-subscription CLI backends.
    #[must_use]
    pub fn discover_cli_fleet(&self) -> Vec<CliToolStatus> {
        discover_cli_fleet()
    }

    /// Partition `nodes` into dependency-ready, scope-disjoint waves using this supervisor's
    /// `max_parallel` concurrency limit.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowEmbedError`] if `wave_id` is empty, a scope is invalid, or dependencies
    /// contain an unknown key or cycle.
    pub fn plan_embedded_workflow(
        &self,
        wave_id: &str,
        nodes: Vec<WorkflowNode>,
    ) -> Result<WorkflowWavePlan, WorkflowEmbedError> {
        EmbeddedWorkflowOrchestrator::new(self.max_parallel).plan_wave(wave_id, &nodes)
    }

    /// Execute `request` through the default [`AdaptiveRunner`].
    #[must_use]
    pub fn execute_adaptive_task(&self, request: &AdaptiveTaskRequest) -> AdaptiveTaskOutcome {
        execute_adaptive_task_with(&self.adaptive_runner(), request)
    }

    /// Execute `request` through a caller-supplied [`AdaptiveRunner`].
    #[must_use]
    pub fn execute_adaptive_task_with<P: Runner, C: Runner>(
        &self,
        runner: &AdaptiveRunner<P, C>,
        request: &AdaptiveTaskRequest,
    ) -> AdaptiveTaskOutcome {
        execute_adaptive_task_with(runner, request)
    }

    /// Build a complete [`AdaptiveSupervisorReport`] of discovered CLI subscription tools,
    /// Susi AI OS Socket endpoints, active adaptive execution policy, and embedded workflow readiness.
    #[must_use]
    pub fn adaptive_status_report(&self) -> AdaptiveSupervisorReport {
        build_adaptive_supervisor_report(0)
    }

    /// Run the plan, skipping tasks an earlier run landed.
    ///
    /// Pending tasks run in waves chosen by the embedded workflow engine
    /// ([`workflow::next_wave`]): a task joins a wave once its dependencies
    /// have landed, and only when its scopes are disjoint from every other
    /// task in the wave, so tasks that reserve the same paths run one after
    /// the other instead of colliding at the claim. Up to `max_parallel`
    /// tasks prepare at once; each wave's results are then landed serially,
    /// in plan order, and persisted one by one. A task whose dependency did
    /// not land fails without being attempted.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the state cannot be read or written, and
    /// `EaiError::config` for an unknown dependency, a dependency cycle, or a
    /// scope that cannot be normalized.
    pub fn run(&self, plan: &Plan) -> EaiResult<SupervisorReport> {
        workflow::validate_dependencies(&plan.tasks)?;
        let mut state = load_state(&self.state_path)?;
        let mut tally = Tally::default();
        let mut skipped = Vec::new();
        let mut remaining: Vec<&PlanTask> = plan
            .tasks
            .iter()
            .filter(|task| {
                let finished = state
                    .records
                    .get(&task.key)
                    .is_some_and(|record| record.status == "done");
                if finished {
                    skipped.push(task.key.clone());
                }
                !finished
            })
            .collect();
        // What an earlier run landed satisfies a dependency this run.
        let mut completed: BTreeSet<String> = skipped.iter().cloned().collect();
        let mut failed: BTreeSet<String> = BTreeSet::new();
        let mut unstarted = Vec::new();
        let mut budget_exhausted = false;
        let mut paused = false;
        let mut aborted = false;
        let parallel = self.max_parallel.max(1);
        loop {
            self.fail_dependents(&mut remaining, &mut failed, &mut state, &mut tally)?;
            if remaining.is_empty() {
                break;
            }
            if let Some(path) = &self.control_path {
                let controls = ControlState::load(path);
                if controls.blocks_new_work() {
                    paused = controls.paused;
                    aborted = controls.aborted;
                    unstarted.extend(remaining.iter().map(|task| task.key.clone()));
                    break;
                }
            }
            let wave = workflow::next_wave(&remaining, &completed, &[], parallel)?;
            if wave.is_empty() {
                // Nothing can start: leave the rest for a later run rather
                // than spin. Validation makes this unreachable for a plan
                // whose dependencies all resolve within it.
                unstarted.extend(remaining.iter().map(|task| task.key.clone()));
                break;
            }
            let admitted: BTreeSet<&str> = wave.iter().map(|task| task.key.as_str()).collect();
            remaining.retain(|task| !admitted.contains(task.key.as_str()));
            for (task, result) in self.prepare_batch(&wave) {
                let status = match result {
                    Ok(prepared) => self.executor.land(prepared),
                    Err(error) => Ok(TaskStatus::Failed {
                        reason: error.to_string(),
                    }),
                }
                .unwrap_or_else(|error| TaskStatus::Failed {
                    reason: error.to_string(),
                });
                let landed = matches!(status, TaskStatus::Done { .. });
                // Once the mission stops for a top-up, the rest of this wave
                // was already prepared. Work that passed still lands: the
                // stop used to break here and leave a wave-mate's accepted
                // commit unmerged with its claim held. Anything else is left
                // for a later run, as before.
                if budget_exhausted {
                    if landed {
                        record(&mut state, &self.state_path, task, status, &mut tally)?;
                        completed.insert(task.key.clone());
                    } else {
                        unstarted.push(task.key.clone());
                    }
                    continue;
                }
                let stop = self.executor.budget_exhausted();
                record(&mut state, &self.state_path, task, status, &mut tally)?;
                if landed {
                    completed.insert(task.key.clone());
                } else {
                    failed.insert(task.key.clone());
                }
                budget_exhausted = stop;
            }
            if budget_exhausted {
                unstarted.extend(remaining.iter().map(|task| task.key.clone()));
                break;
            }
        }
        Ok(SupervisorReport {
            mission: plan.mission.clone(),
            done: tally.done,
            failed: tally.failed,
            skipped,
            unstarted,
            budget_exhausted,
            paused,
            aborted,
        })
    }

    /// Fail, unattempted, every remaining task with a failed dependency —
    /// transitively — and record each one.
    fn fail_dependents(
        &self,
        remaining: &mut Vec<&PlanTask>,
        failed: &mut BTreeSet<String>,
        state: &mut State,
        tally: &mut Tally,
    ) -> EaiResult<()> {
        loop {
            let blocked = remaining.iter().enumerate().find_map(|(position, task)| {
                task.deps
                    .iter()
                    .find(|dep| failed.contains(*dep))
                    .map(|dep| (position, dep.clone()))
            });
            let Some((position, dep)) = blocked else {
                return Ok(());
            };
            let task = remaining.remove(position);
            let status = TaskStatus::Failed {
                reason: format!("dependency {dep} did not complete"),
            };
            record(state, &self.state_path, task, status, tally)?;
            failed.insert(task.key.clone());
        }
    }

    fn prepare_batch<'b>(
        &self,
        batch: &[&'b PlanTask],
    ) -> Vec<(&'b PlanTask, EaiResult<E::Prepared>)> {
        if batch.len() == 1 {
            let task = batch[0];
            return vec![(task, self.executor.prepare(task))];
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|task| (*task, scope.spawn(move || self.executor.prepare(task))))
                .collect();
            handles
                .into_iter()
                .map(|(task, handle)| {
                    let result = handle
                        .join()
                        .unwrap_or_else(|_| Err(EaiError::process("prepare thread panicked")));
                    (task, result)
                })
                .collect()
        })
    }
}

fn record(
    state: &mut State,
    state_path: &Path,
    task: &PlanTask,
    status: TaskStatus,
    tally: &mut Tally,
) -> EaiResult<()> {
    let stored = match &status {
        TaskStatus::Done { task: id } => Record {
            status: "done".into(),
            task: Some(id.clone()),
            reason: None,
        },
        TaskStatus::Failed { reason } => Record {
            status: "failed".into(),
            task: None,
            reason: Some(reason.clone()),
        },
    };
    state.records.insert(task.key.clone(), stored);
    save_state(state_path, state)?;
    match status {
        TaskStatus::Done { .. } => tally.done.push(task.key.clone()),
        TaskStatus::Failed { reason } => tally.failed.push((task.key.clone(), reason)),
    }
    Ok(())
}

/// The production executor: one susi task through the adapter.
pub struct SusiExecutor<'a, R: CommandRunner> {
    /// The prepared target.
    pub target: &'a SusiTarget<R>,
    /// The agent runner.
    pub runner: &'a dyn Runner,
    /// Worktree name prefix.
    pub worktree_prefix: String,
    /// Gateway base URL.
    pub gateway_url: String,
    /// Model id used when a task does not name one.
    pub model: String,
    /// Models to escalate to when the preferred one does not pass.
    pub alternates: Vec<String>,
    /// Price catalogue used to rank alternates and estimate spend.
    pub catalogue: Option<PriceCatalogue>,
    /// Mission budget; when absent the mission is unmetered.
    pub budget: Option<Budget>,
    /// Ledger the gateway writes (spend authority).
    pub ledger_path: PathBuf,
    /// Mission id every attempt is billed to.
    pub mission: String,
    /// Escalation policy.
    pub policy: Policy,
    /// Per-turn deadline.
    pub timeout: Duration,
    /// Per-task adapter state root.
    pub state_dir: PathBuf,
    /// Self-build lifecycle ledger the scorecard reads; `None` disables it.
    pub lifecycle_path: Option<PathBuf>,
    /// Built on the first prepare when a budget is set.
    pub guard: std::sync::OnceLock<BudgetGuard>,
    /// Set once the budget refused an attempt, or a task found no model on its
    /// ladder that can be paid for ([`SusiRunReport::budget_exhausted`]).
    pub exhausted: AtomicBool,
}

impl<R: CommandRunner> TaskExecutor for SusiExecutor<'_, R> {
    type Prepared = SusiRunReport;

    fn prepare(&self, task: &PlanTask) -> EaiResult<SusiRunReport> {
        let preferred = task.model.clone().unwrap_or_else(|| self.model.clone());
        // The mission's default model is a worker on every task, even when the
        // task names a different one: otherwise a task assigned the cheap
        // worker has no stronger rung to escalate to.
        let mut alternates = self.alternates.clone();
        if !alternates.iter().any(|candidate| candidate == &self.model) {
            alternates.push(self.model.clone());
        }
        let spec = SusiTaskSpec {
            worktree_name: format!("{}-{}", self.worktree_prefix, task.key),
            title: task.title.clone(),
            accept: task.accept.clone(),
            scopes: task.scopes.clone(),
            prompt: task.prompt.clone(),
            gateway_url: self.gateway_url.clone(),
            model: preferred,
            alternates,
            timeout: self.timeout,
            state_dir: self.state_dir.join(&task.key),
            task: task.task.clone(),
            precheck: true,
            net_deletion_allowance: crate::adapter::net_deletion_allowance_from_env(),
        };
        let guard = self
            .budget
            .map(|budget| self.guard.get_or_init(|| BudgetGuard::new(budget)));
        let run_policy = SusiRunPolicy {
            catalogue: self.catalogue.as_ref(),
            guard,
            ledger_path: &self.ledger_path,
            mission: &self.mission,
            policy: self.policy,
        };
        let report = run_task(self.target, &spec, self.runner, &run_policy)?;
        if report.budget_exhausted {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        self.emit_work(&report);
        Ok(report)
    }

    fn land(&self, report: SusiRunReport) -> EaiResult<TaskStatus> {
        if !report.accepted {
            return Ok(TaskStatus::Failed {
                reason: format!(
                    "{} attempt(s); {} / committed {} / acceptance not passed",
                    report.attempts.len(),
                    describe(&report.agent),
                    report.committed
                ),
            });
        }
        land(self.target, &report)?;
        self.emit_merged(&report);
        Ok(TaskStatus::Done { task: report.task })
    }

    fn budget_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::SeqCst)
    }
}

impl<R: CommandRunner> SusiExecutor<'_, R> {
    /// Record what the prepare phase proved, before landing.
    fn emit_work(&self, report: &SusiRunReport) {
        let Some(path) = &self.lifecycle_path else {
            return;
        };
        let detail = format!(
            "{} attempt(s); {}",
            report.attempts.len(),
            describe(&report.agent)
        );
        for event in work_events(
            &report.task,
            report.committed,
            report.accepted,
            detail,
            lifecycle::now_unix(),
        ) {
            write_lifecycle(path, &event);
        }
    }

    /// Record the merge and what the task cost, once landing returned.
    ///
    /// `workflow finish` returns only after the accepted close is published, so
    /// this point is the first one where the harness can truthfully say
    /// `merged` rather than `pushed`.
    fn emit_merged(&self, report: &SusiRunReport) {
        let Some(path) = &self.lifecycle_path else {
            return;
        };
        let cost = lifecycle::task_cost(&self.ledger_path, &self.mission, &report.task);
        write_lifecycle(
            path,
            &LifecycleEvent::merged(&report.task, lifecycle::now_unix(), cost),
        );
    }
}

/// A lifecycle write must never fail a mission that otherwise succeeded: the
/// scorecard is evidence, not a gate. A broken path is reported on stderr.
fn write_lifecycle(path: &Path, event: &LifecycleEvent) {
    if let Err(error) = lifecycle::append(path, event) {
        eprintln!("harness: lifecycle write failed: {error}");
    }
}

/// The events a prepared task earns: a committed change is `implemented`, a
/// passing acceptance is `gated`, and anything else is a `defect` carrying the
/// reason. Pure, so the decision is testable without a repository.
fn work_events(
    task: &str,
    committed: bool,
    accepted: bool,
    detail: String,
    at_unix: u64,
) -> Vec<LifecycleEvent> {
    let mut events = Vec::new();
    if committed {
        events.push(LifecycleEvent::new(task, "implemented", at_unix));
    }
    if accepted {
        events.push(LifecycleEvent::new(task, "gated", at_unix));
    } else {
        events.push(LifecycleEvent::defect(
            task,
            at_unix,
            "not-accepted",
            detail,
        ));
    }
    events
}

fn describe(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Succeeded { .. } => "succeeded".to_string(),
        RunOutcome::Failed { message } => format!("failed: {message}"),
        RunOutcome::TimedOut => "timed_out".to_string(),
    }
}

fn load_state(path: &Path) -> EaiResult<State> {
    if !path.is_file() {
        return Ok(State::default());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| EaiError::io(format!("read state {}: {error}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|error| EaiError::io(format!("parse state {}: {error}", path.display())))
}

fn save_state(path: &Path, state: &State) -> EaiResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| EaiError::io(format!("create {}: {error}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| EaiError::io(format!("encode state: {error}")))?;
    // Write a sibling temp file, then rename it over the state. A reader or a
    // crash never sees a half-written state, and two writers never splice.
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, text)
        .map_err(|error| EaiError::io(format!("write state {}: {error}", temp.display())))?;
    std::fs::rename(&temp, path)
        .map_err(|error| EaiError::io(format!("replace state {}: {error}", path.display())))
}

#[cfg(test)]
#[path = "tests/supervisor.rs"]
mod tests;
