//! Recursive container adapter: composable Driver (Coordinator) and Engine roles.
//!
//! Implements CEO Mandate 73 Section 2 ("Recursive adapter: driver and engine are
//! composable roles"):
//! - Any verified Susi container component (`carbon`, `opus`, `grok`, `susi-harness`,
//!   `dot`, or a GMCP/GEMI/GAWD socket node) can act as a **Driver**, an **Engine**,
//!   or **both**, nesting across multiple levels with zero hardcoded dependency on
//!   any single vendor or coordinator identity.
//! - Drivers are admitted strictly by verified runtime capability contracts, never
//!   by assumed vendor presence.
//! - Every parallel or nested worker receives an isolated workspace, private build
//!   target directory, private scratch home, and disjoint Susi 5-port socket offset
//!   so every agent operates in complete isolation and freedom.
//! - Authority, scopes, capabilities, spend (`MicroUsd`), wall-clock timeouts, and
//!   recursion depth attenuate monotonically from parent to child; a child or
//!   wrapper can never enlarge its permissions.
//! - Enforces deterministic `(component_id, task_key)` cycle detection, hierarchical
//!   cooperative cancellation, idempotent retry memoization without fake success,
//!   and SHA-256 parent/child provenance receipts.

use crate::executor::{ExecutorConfig, RunOutcome, Runner};
use crate::money::MicroUsd;
use crate::supervisor::SusiSocketReport;
use crate::workflow::{normalize_scope, scopes_overlap, task_scopes_disjoint};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Versioned schema identifier for recursive Driver/Engine provenance receipts.
pub const RECURSIVE_CONTRACT_SCHEMA: &str = "susi-recursive-container-v1";

/// Hard upper bound on recursion depth across nested Driver <-> Engine calls.
pub const MAX_RECURSION_DEPTH_LIMIT: u8 = 8;

/// Minimum port stride between concurrent isolated workers (5-port Susi socket `9090..9094`).
pub const MIN_ISOLATED_PORT_STRIDE: u16 = 10;

/// Contextual role a Susi container component plays in an execution frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRole {
    /// Coordinates waves of child components without running leaf execution itself.
    Driver,
    /// Executes leaf work within its isolated sandbox.
    Engine,
    /// Composable dual role: can both coordinate child waves as a Driver and
    /// execute leaf turns as an Engine.
    DriverAndEngine,
}

impl ContainerRole {
    /// True when this role may coordinate child waves.
    #[must_use]
    pub const fn can_drive(self) -> bool {
        matches!(self, Self::Driver | Self::DriverAndEngine)
    }

    /// True when this role may execute leaf work directly.
    #[must_use]
    pub const fn can_execute(self) -> bool {
        matches!(self, Self::Engine | Self::DriverAndEngine)
    }

    /// Stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Driver => "driver",
            Self::Engine => "engine",
            Self::DriverAndEngine => "driver_and_engine",
        }
    }
}

/// Versioned capability contract advertised by a container component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityContract {
    /// Contract schema version (`"susi-recursive-container-v1"`).
    pub contract_version: String,
    /// Unique component identity (`"carbon-antigravity"`, `"opus-claude"`, `"susi-harness"`, etc.).
    pub component_id: String,
    /// Role capability (`Driver`, `Engine`, or `DriverAndEngine`).
    pub role: ContainerRole,
    /// Verified capabilities exposed by this container (e.g. `"wave_plan"`, `"code_edit"`, `"distill"`).
    pub capabilities: BTreeSet<String>,
    /// True only when runtime availability was actively verified on this host/session.
    pub verified_available: bool,
}

impl CapabilityContract {
    /// Verify structural contract invariants.
    pub fn validate(&self) -> Result<(), RecursiveError> {
        if self.contract_version != RECURSIVE_CONTRACT_SCHEMA {
            return Err(RecursiveError::InvalidContract(format!(
                "unsupported contract_version {:?}, expected {RECURSIVE_CONTRACT_SCHEMA}",
                self.contract_version
            )));
        }
        if self.component_id.trim().is_empty() {
            return Err(RecursiveError::InvalidContract(
                "component_id must be non-empty".to_string(),
            ));
        }
        if self.capabilities.is_empty() || self.capabilities.iter().any(|cap| cap.trim().is_empty())
        {
            return Err(RecursiveError::InvalidContract(format!(
                "component {} must declare non-empty capabilities",
                self.component_id
            )));
        }
        Ok(())
    }
}

/// Select the first verified, available Driver whose contract satisfies all
/// `required_capabilities`. Never assumes `dot` or any fixed vendor is online.
pub fn select_verified_driver<'a>(
    candidates: &'a [CapabilityContract],
    required_capabilities: &[&str],
) -> Result<&'a CapabilityContract, RecursiveError> {
    for candidate in candidates {
        candidate.validate()?;
        if !candidate.verified_available || !candidate.role.can_drive() {
            continue;
        }
        let covers_all = required_capabilities
            .iter()
            .all(|req| candidate.capabilities.contains(*req));
        if covers_all {
            return Ok(candidate);
        }
    }
    Err(RecursiveError::NoVerifiedDriverAvailable {
        required: required_capabilities
            .iter()
            .map(|cap| (*cap).to_string())
            .collect(),
    })
}

/// Monotonically attenuating authority, scope, budget, timeout, and depth grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityGrant {
    /// Normalized repository-relative scopes authorized in this frame.
    pub allowed_scopes: Vec<String>,
    /// Capabilities authorized in this frame.
    pub allowed_capabilities: BTreeSet<String>,
    /// Whether outbound network calls are permitted.
    pub allow_network: bool,
    /// Whether Git ref publication is permitted.
    pub allow_git_publish: bool,
    /// Remaining recursion depth allowed strictly below this frame (`<= 8`).
    pub max_depth: u8,
    /// Maximum spending cap (`MicroUsd`) for this frame and all nested children.
    pub budget_cap: MicroUsd,
    /// Maximum wall-clock timeout in milliseconds.
    pub timeout_ms: u64,
}

impl AuthorityGrant {
    /// Normalize scopes and verify bounds on a root or child grant.
    pub fn normalized(&self) -> Result<Self, RecursiveError> {
        if self.max_depth > MAX_RECURSION_DEPTH_LIMIT {
            return Err(RecursiveError::DepthLimitExceeded {
                requested: self.max_depth,
                limit: MAX_RECURSION_DEPTH_LIMIT,
            });
        }
        if self.timeout_ms == 0 {
            return Err(RecursiveError::InvalidContract(
                "timeout_ms must be > 0".to_string(),
            ));
        }
        if self.allowed_scopes.is_empty() {
            return Err(RecursiveError::AuthorityEscalation(
                "allowed_scopes cannot be empty".to_string(),
            ));
        }
        let mut normalized_scopes = Vec::with_capacity(self.allowed_scopes.len());
        for raw in &self.allowed_scopes {
            let norm = normalize_scope(raw)
                .map_err(|err| RecursiveError::InvalidContract(err.to_string()))?;
            normalized_scopes.push(norm);
        }
        Ok(Self {
            allowed_scopes: normalized_scopes,
            allowed_capabilities: self.allowed_capabilities.clone(),
            allow_network: self.allow_network,
            allow_git_publish: self.allow_git_publish,
            max_depth: self.max_depth,
            budget_cap: self.budget_cap,
            timeout_ms: self.timeout_ms,
        })
    }

    /// Verify that `child` does not enlarge any permission, scope, depth,
    /// timeout, or budget relative to `self`.
    pub fn delegate(&self, child: &Self) -> Result<Self, RecursiveError> {
        let parent = self.normalized()?;
        let child_norm = child.normalized()?;

        if parent.max_depth == 0 || child_norm.max_depth >= parent.max_depth {
            return Err(RecursiveError::DepthLimitExceeded {
                requested: child_norm.max_depth,
                limit: parent.max_depth.saturating_sub(1),
            });
        }
        if child_norm.allow_network && !parent.allow_network {
            return Err(RecursiveError::AuthorityEscalation(
                "child requested allow_network=true when parent forbids network".to_string(),
            ));
        }
        if child_norm.allow_git_publish && !parent.allow_git_publish {
            return Err(RecursiveError::AuthorityEscalation(
                "child requested allow_git_publish=true when parent forbids git publish"
                    .to_string(),
            ));
        }
        if let Some(extra_cap) = child_norm
            .allowed_capabilities
            .difference(&parent.allowed_capabilities)
            .next()
        {
            return Err(RecursiveError::AuthorityEscalation(format!(
                "child requested ungranted capability {extra_cap:?}"
            )));
        }
        for child_scope in &child_norm.allowed_scopes {
            let covered = parent
                .allowed_scopes
                .iter()
                .any(|parent_scope| scope_is_within(child_scope, parent_scope));
            if !covered {
                return Err(RecursiveError::AuthorityEscalation(format!(
                    "child scope {child_scope:?} escapes parent scopes {:?}",
                    parent.allowed_scopes
                )));
            }
        }
        if child_norm.budget_cap > parent.budget_cap {
            return Err(RecursiveError::BudgetExceeded {
                requested: child_norm.budget_cap,
                available: parent.budget_cap,
            });
        }
        if child_norm.timeout_ms > parent.timeout_ms {
            return Err(RecursiveError::AuthorityEscalation(format!(
                "child timeout_ms {} exceeds parent timeout_ms {}",
                child_norm.timeout_ms, parent.timeout_ms
            )));
        }
        Ok(child_norm)
    }
}

fn scope_is_within(child_norm: &str, parent_norm: &str) -> bool {
    child_norm == parent_norm
        || child_norm
            .strip_prefix(parent_norm)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Isolated execution sandbox assigned to a single worker so concurrent agents
/// never share writable worktrees, build targets, scratch homes, or ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerIsolationSandbox {
    /// Worker session identifier (`"host-session:agent:depth-slot"`).
    pub worker_identity: String,
    /// Dedicated worktree directory.
    pub worktree_dir: PathBuf,
    /// Dedicated Cargo/compiler output directory.
    pub target_dir: PathBuf,
    /// Dedicated hermetic `HOME`/`SUSI_HOME` scratch directory.
    pub scratch_home: PathBuf,
    /// Dedicated Susi 5-port socket allocation (`9090 + port_offset`, etc.).
    pub sockets: SusiSocketReport,
    /// Assigned port offset.
    pub port_offset: u16,
}

/// Allocate strictly disjoint [`WorkerIsolationSandbox`]es for a wave of workers
/// under `sandbox_root`, separated by at least [`MIN_ISOLATED_PORT_STRIDE`].
pub fn allocate_wave_sandboxes(
    sandbox_root: &Path,
    run_prefix: &str,
    worker_keys: &[String],
    base_port_offset: u16,
    port_stride: u16,
) -> Result<Vec<WorkerIsolationSandbox>, RecursiveError> {
    if port_stride < MIN_ISOLATED_PORT_STRIDE {
        return Err(RecursiveError::InvalidContract(format!(
            "port_stride {port_stride} must be >= {MIN_ISOLATED_PORT_STRIDE} to isolate 5-port sockets"
        )));
    }
    let mut seen_keys = BTreeSet::new();
    let mut sandboxes = Vec::with_capacity(worker_keys.len());
    for (idx, raw_key) in worker_keys.iter().enumerate() {
        let slug = sanitize_slug(raw_key)?;
        if !seen_keys.insert(slug.clone()) {
            return Err(RecursiveError::IsolationConflict(format!(
                "duplicate worker key {slug:?} in wave sandbox allocation"
            )));
        }
        let idx_u16 = u16::try_from(idx).map_err(|_| {
            RecursiveError::InvalidContract("wave worker count exceeds u16 port range".to_string())
        })?;
        let step = idx_u16.checked_mul(port_stride).ok_or_else(|| {
            RecursiveError::InvalidContract("port offset multiplication overflow".to_string())
        })?;
        let port_offset = base_port_offset.checked_add(step).ok_or_else(|| {
            RecursiveError::InvalidContract("port offset addition overflow".to_string())
        })?;
        let base_dir = sandbox_root.join(format!("{run_prefix}-{slug}"));
        sandboxes.push(WorkerIsolationSandbox {
            worker_identity: format!("{run_prefix}:{slug}"),
            worktree_dir: base_dir.join("worktree"),
            target_dir: base_dir.join("target"),
            scratch_home: base_dir.join("scratch-home"),
            sockets: SusiSocketReport::from_port_offset(port_offset),
            port_offset,
        });
    }
    Ok(sandboxes)
}

fn sanitize_slug(raw: &str) -> Result<String, RecursiveError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(RecursiveError::InvalidContract(format!(
            "identifier {raw:?} must contain only ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Hierarchical cooperative cancellation token propagating parent cancellation
/// immediately to every nested child Engine and Driver.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<CancellationToken>>>,
}

impl CancellationToken {
    /// Create an uncancelled root token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive a child token linked to `self`: cancelling `self` cancels the
    /// child, while cancelling the child alone does not cancel siblings.
    #[must_use]
    pub fn child_token(&self) -> Self {
        let child = Self::new();
        if self.is_cancelled() {
            child.cancel();
        }
        if let Ok(mut guard) = self.children.lock() {
            guard.push(child.clone());
        }
        child
    }

    /// Cancel this token and recursively signal all registered child tokens.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(guard) = self.children.lock() {
            for child in guard.iter() {
                child.cancel();
            }
        }
    }

    /// True if cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Memoization store preventing duplicate side effects on idempotent retries of
/// already-succeeded work while never caching or masking failures.
#[derive(Debug, Clone, Default)]
pub struct IdempotencyLedger {
    succeeded: Arc<Mutex<BTreeMap<String, LeafExecutionOutput>>>,
}

impl IdempotencyLedger {
    /// Create an empty idempotency ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Deterministic idempotency key from `(component_id, task_key, prompt)`.
    #[must_use]
    pub fn compute_key(component_id: &str, task_key: &str, prompt: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"susi-recursive-idem-v1|");
        hasher.update(component_id.as_bytes());
        hasher.update(b"|");
        hasher.update(task_key.as_bytes());
        hasher.update(b"|");
        hasher.update(prompt.as_bytes());
        susi_paths::hex::encode(hasher.finalize())
    }

    fn lookup(&self, key: &str) -> Option<LeafExecutionOutput> {
        self.succeeded
            .lock()
            .ok()
            .and_then(|map| map.get(key).cloned())
    }

    fn record_success(&self, key: String, output: LeafExecutionOutput) {
        if let Ok(mut map) = self.succeeded.lock() {
            map.insert(key, output);
        }
    }
}

/// Output produced by a leaf Engine execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafExecutionOutput {
    /// Summary of completed work.
    pub summary: String,
    /// Actual metered PAYG spend (`MicroUsd::ZERO` for subscription CLIs or reflexes).
    pub spent: MicroUsd,
    /// SHA-256 digest of verified artifacts or test output.
    pub output_digest: String,
}

/// Recursive task specification: either a leaf Engine turn or a Driver wave
/// containing nested [`RecursiveTaskNode`]s (which may themselves be Drivers!).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecursiveTaskNode {
    /// Unique task key within its wave.
    pub task_key: String,
    /// Component id to execute or drive this node.
    pub component_id: String,
    /// Required capabilities for component admission.
    pub required_capabilities: Vec<String>,
    /// Prompt or objective passed to this node.
    pub prompt: String,
    /// Delegated authority for this node.
    pub grant: AuthorityGrant,
    /// Optional nested child waves when this node acts as a Driver.
    #[serde(default)]
    pub child_waves: Vec<Vec<RecursiveTaskNode>>,
}

/// Context passed into [`ContainerComponent::execute_leaf`] for an isolated Engine turn.
#[derive(Debug, Clone)]
pub struct EngineTurnContext<'a> {
    /// Unique traceable run id (`parent_run_id/task_key`).
    pub run_id: &'a str,
    /// Parent run id (`None` at root).
    pub parent_run_id: Option<&'a str>,
    /// Recursion depth (`0` at root).
    pub depth: u8,
    /// Task key.
    pub task_key: &'a str,
    /// Task prompt.
    pub prompt: &'a str,
    /// Verified authority grant.
    pub grant: &'a AuthorityGrant,
    /// Dedicated isolated sandbox.
    pub sandbox: &'a WorkerIsolationSandbox,
    /// Cooperative cancellation handle.
    pub cancel: &'a CancellationToken,
}

/// Trait implemented by any Susi container component (harness runner, socket
/// bridge, distillation engine, or test fixture).
pub trait ContainerComponent: Send + Sync {
    /// Component's capability contract.
    fn contract(&self) -> &CapabilityContract;

    /// Execute a leaf Engine turn inside `ctx.sandbox`.
    fn execute_leaf(&self, ctx: &EngineTurnContext<'_>) -> Result<LeafExecutionOutput, String>;
}

/// Bridge adapting any `susi-harness` [`Runner`] (`AdaptiveRunner`, `NativeWorker`,
/// `CliSubRunner`) into a composable [`ContainerComponent`].
pub struct RunnerContainerBridge<R> {
    contract: CapabilityContract,
    runner: R,
    gateway_url: String,
    model: String,
}

impl<R: Runner + Send + Sync> RunnerContainerBridge<R> {
    /// Wrap `runner` with an explicit [`CapabilityContract`].
    #[must_use]
    pub fn new(
        contract: CapabilityContract,
        runner: R,
        gateway_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            contract,
            runner,
            gateway_url: gateway_url.into(),
            model: model.into(),
        }
    }
}

impl<R: Runner + Send + Sync> ContainerComponent for RunnerContainerBridge<R> {
    fn contract(&self) -> &CapabilityContract {
        &self.contract
    }

    fn execute_leaf(&self, ctx: &EngineTurnContext<'_>) -> Result<LeafExecutionOutput, String> {
        if ctx.cancel.is_cancelled() {
            return Err("cancelled before runner start".to_string());
        }
        let cfg = ExecutorConfig {
            gateway_url: self.gateway_url.clone(),
            model: self.model.clone(),
            workspace: ctx.sandbox.worktree_dir.clone(),
            state_dir: ctx.sandbox.scratch_home.clone(),
            mission: ctx.parent_run_id.unwrap_or("root").to_string(),
            task: ctx.task_key.to_string(),
            attempt: 1,
            timeout: Duration::from_millis(ctx.grant.timeout_ms),
            prompt: ctx.prompt.to_string(),
        };
        match self.runner.run(&cfg) {
            Ok(RunOutcome::Succeeded { final_text, .. }) => {
                let mut hasher = Sha256::new();
                hasher.update(final_text.as_bytes());
                Ok(LeafExecutionOutput {
                    summary: final_text,
                    spent: MicroUsd::ZERO,
                    output_digest: susi_paths::hex::encode(hasher.finalize()),
                })
            }
            Ok(RunOutcome::Failed { message }) => Err(format!("runner failed: {message}")),
            Ok(RunOutcome::TimedOut) => Err(format!(
                "runner timed out after {} ms",
                ctx.grant.timeout_ms
            )),
            Err(err) => Err(err.to_string()),
        }
    }
}

/// Errors returned by recursive Driver <-> Engine admission, delegation, or execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecursiveError {
    /// Contract fields failed validation.
    InvalidContract(String),
    /// Component is unregistered, unverified, or lacks required capabilities/role.
    NoVerifiedDriverAvailable {
        /// Capabilities that could not be satisfied.
        required: Vec<String>,
    },
    /// Component could not be admitted for the requested role or capability.
    ComponentRejected {
        /// Component id.
        component_id: String,
        /// Rejection reason.
        reason: String,
    },
    /// Child attempted to enlarge authority, scopes, or timeout beyond parent.
    AuthorityEscalation(String),
    /// Recursion depth limit exceeded.
    DepthLimitExceeded {
        /// Requested remaining depth.
        requested: u8,
        /// Allowed remaining depth.
        limit: u8,
    },
    /// Budget cap exceeded.
    BudgetExceeded {
        /// Requested or spent amount.
        requested: MicroUsd,
        /// Available cap.
        available: MicroUsd,
    },
    /// Concurrent sibling workers collided on scopes or sandbox identities.
    IsolationConflict(String),
    /// Cycle detected in `(component_id, task_key)` call stack.
    CycleDetected(String),
    /// Cooperative cancellation triggered.
    Cancelled {
        /// Run id where cancellation was observed.
        run_id: String,
    },
    /// Honest propagation of an Engine or child Driver execution failure.
    ExecutionFailed {
        /// Run id that failed.
        run_id: String,
        /// Underlying failure detail.
        reason: String,
    },
}

impl std::fmt::Display for RecursiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidContract(msg) => write!(f, "invalid recursive contract: {msg}"),
            Self::NoVerifiedDriverAvailable { required } => {
                write!(
                    f,
                    "no verified available driver satisfies capabilities {required:?}"
                )
            }
            Self::ComponentRejected {
                component_id,
                reason,
            } => write!(f, "component {component_id:?} rejected: {reason}"),
            Self::AuthorityEscalation(msg) => write!(f, "authority escalation refused: {msg}"),
            Self::DepthLimitExceeded { requested, limit } => {
                write!(
                    f,
                    "recursion depth {requested} exceeds allowed limit {limit}"
                )
            }
            Self::BudgetExceeded {
                requested,
                available,
            } => write!(f, "budget {requested} exceeds available cap {available}"),
            Self::IsolationConflict(msg) => write!(f, "worker isolation conflict: {msg}"),
            Self::CycleDetected(chain) => write!(f, "recursive call cycle detected: {chain}"),
            Self::Cancelled { run_id } => write!(f, "execution cancelled at {run_id}"),
            Self::ExecutionFailed { run_id, reason } => {
                write!(f, "execution failed at {run_id}: {reason}")
            }
        }
    }
}

impl std::error::Error for RecursiveError {}

/// Traceable provenance receipt for a single Driver or Engine frame and all
/// nested children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionProvenanceReceipt {
    /// Receipt schema (`"susi-recursive-container-v1"`).
    pub schema: String,
    /// Unique run identifier (`"root/t1/sub1"`).
    pub run_id: String,
    /// Parent run identifier (`None` for the root frame).
    pub parent_run_id: Option<String>,
    /// Nesting depth (`0` at root).
    pub depth: u8,
    /// Component that executed or drove this frame.
    pub component_id: String,
    /// Effective role (`"driver"` or `"engine"`).
    pub effective_role: ContainerRole,
    /// Task key.
    pub task_key: String,
    /// Whether output was served from the idempotent retry ledger.
    pub replayed_from_idempotency: bool,
    /// Dedicated worker isolation sandbox.
    pub sandbox: WorkerIsolationSandbox,
    /// Total metered spend across this frame and its children.
    pub total_spent: MicroUsd,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Summary produced by this frame.
    pub summary: String,
    /// Nested child provenance receipts in deterministic wave order.
    pub children: Vec<ExecutionProvenanceReceipt>,
    /// Content-addressed SHA-256 hex digest binding parent/child identities,
    /// sandbox, spend, and child digests.
    pub evidence_digest: String,
}

impl ExecutionProvenanceReceipt {
    /// Recompute and verify the recursive SHA-256 `evidence_digest` tree.
    #[must_use]
    pub fn verify_integrity(&self) -> bool {
        if self.schema != RECURSIVE_CONTRACT_SCHEMA {
            return false;
        }
        for child in &self.children {
            if !child.verify_integrity() || child.parent_run_id.as_deref() != Some(&self.run_id) {
                return false;
            }
        }
        self.compute_digest() == self.evidence_digest
    }

    fn compute_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.schema.as_bytes());
        hasher.update(b"|");
        hasher.update(self.run_id.as_bytes());
        hasher.update(b"|");
        hasher.update(self.parent_run_id.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"|");
        hasher.update([self.depth]);
        hasher.update(b"|");
        hasher.update(self.component_id.as_bytes());
        hasher.update(b"|");
        hasher.update(self.effective_role.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(self.task_key.as_bytes());
        hasher.update(b"|");
        hasher.update(self.sandbox.worker_identity.as_bytes());
        hasher.update(b"|");
        hasher.update(self.sandbox.port_offset.to_le_bytes());
        hasher.update(b"|");
        hasher.update(self.total_spent.micros().to_le_bytes());
        hasher.update(b"|");
        hasher.update(self.summary.as_bytes());
        for child in &self.children {
            hasher.update(b"|child:");
            hasher.update(child.evidence_digest.as_bytes());
        }
        susi_paths::hex::encode(hasher.finalize())
    }
}

/// Registry-backed recursive orchestrator executing nested Driver <-> Engine trees.
pub struct RecursiveRuntime {
    components: BTreeMap<String, Arc<dyn ContainerComponent>>,
    sandbox_root: PathBuf,
    port_stride: u16,
    idempotency: IdempotencyLedger,
}

impl RecursiveRuntime {
    /// Create a runtime rooted at `sandbox_root`.
    pub fn new(sandbox_root: impl Into<PathBuf>, port_stride: u16) -> Result<Self, RecursiveError> {
        if port_stride < MIN_ISOLATED_PORT_STRIDE {
            return Err(RecursiveError::InvalidContract(format!(
                "port_stride {port_stride} must be >= {MIN_ISOLATED_PORT_STRIDE}"
            )));
        }
        Ok(Self {
            components: BTreeMap::new(),
            sandbox_root: sandbox_root.into(),
            port_stride,
            idempotency: IdempotencyLedger::new(),
        })
    }

    /// Register a composable container component (Driver, Engine, or both).
    pub fn register(
        &mut self,
        component: Arc<dyn ContainerComponent>,
    ) -> Result<(), RecursiveError> {
        component.contract().validate()?;
        let id = component.contract().component_id.clone();
        self.components.insert(id, component);
        Ok(())
    }

    /// Snapshot all registered contracts in deterministic order.
    #[must_use]
    pub fn contracts(&self) -> Vec<CapabilityContract> {
        self.components
            .values()
            .map(|c| c.contract().clone())
            .collect()
    }

    /// Select the highest-priority verified Driver from `preferred_ids` (or all
    /// registered components when `preferred_ids` is empty) covering
    /// `required_capabilities`.
    pub fn choose_driver(
        &self,
        preferred_ids: &[&str],
        required_capabilities: &[&str],
    ) -> Result<CapabilityContract, RecursiveError> {
        let ordered: Vec<CapabilityContract> = if preferred_ids.is_empty() {
            self.contracts()
        } else {
            preferred_ids
                .iter()
                .filter_map(|id| self.components.get(*id).map(|c| c.contract().clone()))
                .collect()
        };
        select_verified_driver(&ordered, required_capabilities).cloned()
    }

    /// Execute a root [`RecursiveTaskNode`] (which may be an Engine or a multi-level
    /// nested Driver -> Engine -> Driver -> Engine tree).
    pub fn execute(
        &self,
        root_run_id: &str,
        node: &RecursiveTaskNode,
        base_port_offset: u16,
        cancel: &CancellationToken,
    ) -> Result<ExecutionProvenanceReceipt, RecursiveError> {
        let root_slug = sanitize_slug(root_run_id)?;
        let root_grant = node.grant.normalized()?;
        let sandboxes = allocate_wave_sandboxes(
            &self.sandbox_root,
            &root_slug,
            std::slice::from_ref(&node.task_key),
            base_port_offset,
            self.port_stride,
        )?;
        let Some(root_sandbox) = sandboxes.into_iter().next() else {
            return Err(RecursiveError::InvalidContract(
                "missing root sandbox allocation".to_string(),
            ));
        };
        let mut call_stack = Vec::new();
        let mut next_port_cursor = base_port_offset
            .checked_add(self.port_stride)
            .ok_or_else(|| RecursiveError::InvalidContract("port cursor overflow".to_string()))?;

        self.execute_frame(
            &root_slug,
            None,
            0,
            node,
            &root_grant,
            root_sandbox,
            cancel,
            &mut call_stack,
            &mut next_port_cursor,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_frame(
        &self,
        run_id: &str,
        parent_run_id: Option<&str>,
        depth: u8,
        node: &RecursiveTaskNode,
        grant: &AuthorityGrant,
        sandbox: WorkerIsolationSandbox,
        cancel: &CancellationToken,
        call_stack: &mut Vec<(String, String)>,
        next_port_cursor: &mut u16,
    ) -> Result<ExecutionProvenanceReceipt, RecursiveError> {
        if cancel.is_cancelled() {
            return Err(RecursiveError::Cancelled {
                run_id: run_id.to_string(),
            });
        }
        if depth > MAX_RECURSION_DEPTH_LIMIT {
            return Err(RecursiveError::DepthLimitExceeded {
                requested: depth,
                limit: MAX_RECURSION_DEPTH_LIMIT,
            });
        }

        let frame_key = (node.component_id.clone(), node.task_key.clone());
        if call_stack.contains(&frame_key) {
            let mut chain: Vec<String> = call_stack
                .iter()
                .map(|(comp, task)| format!("{comp}:{task}"))
                .collect();
            chain.push(format!("{}:{}", frame_key.0, frame_key.1));
            return Err(RecursiveError::CycleDetected(chain.join(" -> ")));
        }

        let component = self.admit_component(
            &node.component_id,
            &node.required_capabilities,
            !node.child_waves.is_empty(),
            grant,
        )?;

        call_stack.push(frame_key);
        let started = Instant::now();
        let result = if node.child_waves.is_empty() {
            self.run_leaf_engine(
                component.as_ref(),
                run_id,
                parent_run_id,
                depth,
                node,
                grant,
                sandbox,
                cancel,
                started,
            )
        } else {
            self.run_driver_waves(
                run_id,
                parent_run_id,
                depth,
                node,
                grant,
                sandbox,
                cancel,
                call_stack,
                next_port_cursor,
                started,
            )
        };
        call_stack.pop();
        result
    }

    fn admit_component(
        &self,
        component_id: &str,
        required_capabilities: &[String],
        needs_driver_role: bool,
        grant: &AuthorityGrant,
    ) -> Result<Arc<dyn ContainerComponent>, RecursiveError> {
        let Some(component) = self.components.get(component_id) else {
            return Err(RecursiveError::ComponentRejected {
                component_id: component_id.to_string(),
                reason: "component is not registered".to_string(),
            });
        };
        let contract = component.contract();
        contract.validate()?;
        if !contract.verified_available {
            return Err(RecursiveError::ComponentRejected {
                component_id: component_id.to_string(),
                reason: "component availability is not verified on this host".to_string(),
            });
        }
        if needs_driver_role && !contract.role.can_drive() {
            return Err(RecursiveError::ComponentRejected {
                component_id: component_id.to_string(),
                reason: format!("role {} cannot drive nested waves", contract.role.as_str()),
            });
        }
        if !needs_driver_role && !contract.role.can_execute() {
            return Err(RecursiveError::ComponentRejected {
                component_id: component_id.to_string(),
                reason: format!("role {} cannot execute leaf work", contract.role.as_str()),
            });
        }
        for req in required_capabilities {
            if !grant.allowed_capabilities.contains(req) {
                return Err(RecursiveError::AuthorityEscalation(format!(
                    "task requested capability {req:?} not present in frame AuthorityGrant"
                )));
            }
            if !contract.capabilities.contains(req) {
                return Err(RecursiveError::ComponentRejected {
                    component_id: component_id.to_string(),
                    reason: format!("component does not expose required capability {req:?}"),
                });
            }
        }
        Ok(Arc::clone(component))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_leaf_engine(
        &self,
        component: &dyn ContainerComponent,
        run_id: &str,
        parent_run_id: Option<&str>,
        depth: u8,
        node: &RecursiveTaskNode,
        grant: &AuthorityGrant,
        sandbox: WorkerIsolationSandbox,
        cancel: &CancellationToken,
        started: Instant,
    ) -> Result<ExecutionProvenanceReceipt, RecursiveError> {
        let idem_key =
            IdempotencyLedger::compute_key(&node.component_id, &node.task_key, &node.prompt);
        let (leaf_out, replayed) = if let Some(cached) = self.idempotency.lookup(&idem_key) {
            (cached, true)
        } else {
            let child_cancel = cancel.child_token();
            let ctx = EngineTurnContext {
                run_id,
                parent_run_id,
                depth,
                task_key: &node.task_key,
                prompt: &node.prompt,
                grant,
                sandbox: &sandbox,
                cancel: &child_cancel,
            };
            let output = component.execute_leaf(&ctx).map_err(|reason| {
                if child_cancel.is_cancelled() {
                    RecursiveError::Cancelled {
                        run_id: run_id.to_string(),
                    }
                } else {
                    RecursiveError::ExecutionFailed {
                        run_id: run_id.to_string(),
                        reason,
                    }
                }
            })?;
            if child_cancel.is_cancelled() {
                return Err(RecursiveError::Cancelled {
                    run_id: run_id.to_string(),
                });
            }
            if output.spent > grant.budget_cap {
                return Err(RecursiveError::BudgetExceeded {
                    requested: output.spent,
                    available: grant.budget_cap,
                });
            }
            self.idempotency.record_success(idem_key, output.clone());
            (output, false)
        };

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut receipt = ExecutionProvenanceReceipt {
            schema: RECURSIVE_CONTRACT_SCHEMA.to_string(),
            run_id: run_id.to_string(),
            parent_run_id: parent_run_id.map(str::to_string),
            depth,
            component_id: node.component_id.clone(),
            effective_role: ContainerRole::Engine,
            task_key: node.task_key.clone(),
            replayed_from_idempotency: replayed,
            sandbox,
            total_spent: leaf_out.spent,
            duration_ms,
            summary: leaf_out.summary,
            children: Vec::new(),
            evidence_digest: String::new(),
        };
        receipt.evidence_digest = receipt.compute_digest();
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_driver_waves(
        &self,
        run_id: &str,
        parent_run_id: Option<&str>,
        depth: u8,
        node: &RecursiveTaskNode,
        grant: &AuthorityGrant,
        sandbox: WorkerIsolationSandbox,
        cancel: &CancellationToken,
        call_stack: &mut Vec<(String, String)>,
        next_port_cursor: &mut u16,
        started: Instant,
    ) -> Result<ExecutionProvenanceReceipt, RecursiveError> {
        let next_depth = depth
            .checked_add(1)
            .ok_or(RecursiveError::DepthLimitExceeded {
                requested: u8::MAX,
                limit: MAX_RECURSION_DEPTH_LIMIT,
            })?;
        let mut cumulative_spent = MicroUsd::ZERO;
        let mut child_receipts = Vec::new();

        for (wave_idx, wave) in node.child_waves.iter().enumerate() {
            if wave.is_empty() {
                return Err(RecursiveError::InvalidContract(format!(
                    "driver {} wave {wave_idx} cannot be empty",
                    node.task_key
                )));
            }
            // Enforce pairwise scope disjointness across every concurrent sibling in this wave.
            let mut normalized_wave_grants = Vec::with_capacity(wave.len());
            for (idx, child_node) in wave.iter().enumerate() {
                let delegated = grant.delegate(&child_node.grant)?;
                for (prev_idx, prev_grant) in normalized_wave_grants.iter().enumerate() {
                    let prev_grant: &AuthorityGrant = prev_grant;
                    if !task_scopes_disjoint(&prev_grant.allowed_scopes, &delegated.allowed_scopes)
                        || prev_grant.allowed_scopes.iter().any(|left| {
                            delegated
                                .allowed_scopes
                                .iter()
                                .any(|right| scopes_overlap(left, right))
                        })
                    {
                        let left_name = wave
                            .get(prev_idx)
                            .map_or("unknown", |n| n.task_key.as_str());
                        return Err(RecursiveError::IsolationConflict(format!(
                            "wave {wave_idx} siblings {left_name:?} and {:?} have overlapping scopes",
                            child_node.task_key
                        )));
                    }
                }
                let _ = idx;
                normalized_wave_grants.push(delegated);
            }

            let worker_keys: Vec<String> = wave.iter().map(|n| n.task_key.clone()).collect();
            let wave_prefix = format!("{}-w{}", sanitize_slug(&node.task_key)?, wave_idx);
            let wave_sandboxes = allocate_wave_sandboxes(
                &self.sandbox_root,
                &wave_prefix,
                &worker_keys,
                *next_port_cursor,
                self.port_stride,
            )?;
            let wave_count = u16::try_from(wave.len()).map_err(|_| {
                RecursiveError::InvalidContract("wave size exceeds u16".to_string())
            })?;
            let advance = wave_count.checked_mul(self.port_stride).ok_or_else(|| {
                RecursiveError::InvalidContract("port cursor overflow".to_string())
            })?;
            *next_port_cursor = next_port_cursor.checked_add(advance).ok_or_else(|| {
                RecursiveError::InvalidContract("port cursor overflow".to_string())
            })?;

            for ((child_node, child_grant), child_sandbox) in
                wave.iter().zip(normalized_wave_grants).zip(wave_sandboxes)
            {
                let child_cancel = cancel.child_token();
                let child_run_id = format!("{run_id}/{}", sanitize_slug(&child_node.task_key)?);
                let child_receipt = self.execute_frame(
                    &child_run_id,
                    Some(run_id),
                    next_depth,
                    child_node,
                    &child_grant,
                    child_sandbox,
                    &child_cancel,
                    call_stack,
                    next_port_cursor,
                )?;
                cumulative_spent = cumulative_spent
                    .checked_add(child_receipt.total_spent)
                    .ok_or(RecursiveError::BudgetExceeded {
                        requested: MicroUsd::from_micros(u64::MAX),
                        available: grant.budget_cap,
                    })?;
                if cumulative_spent > grant.budget_cap {
                    return Err(RecursiveError::BudgetExceeded {
                        requested: cumulative_spent,
                        available: grant.budget_cap,
                    });
                }
                child_receipts.push(child_receipt);
            }
        }

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let summary = format!(
            "drove {} wave(s), {} child container(s)",
            node.child_waves.len(),
            child_receipts.len()
        );
        let mut receipt = ExecutionProvenanceReceipt {
            schema: RECURSIVE_CONTRACT_SCHEMA.to_string(),
            run_id: run_id.to_string(),
            parent_run_id: parent_run_id.map(str::to_string),
            depth,
            component_id: node.component_id.clone(),
            effective_role: ContainerRole::Driver,
            task_key: node.task_key.clone(),
            replayed_from_idempotency: false,
            sandbox,
            total_spent: cumulative_spent,
            duration_ms,
            summary,
            children: child_receipts,
            evidence_digest: String::new(),
        };
        receipt.evidence_digest = receipt.compute_digest();
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "tests/recursive.rs"]
mod tests;
