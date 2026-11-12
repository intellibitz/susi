//! Native parallel multi-agent workflow engine with remote pull & merge on every step.
//!
//! Implements Mandate 74 workflow invariants for `susi-harness`:
//! - Every workflow step (`start`, `sync`/`step`, `commit`, `distill`, `ship`,
//!   `release`) performs pure-Git remote pull & merge reconciliation
//!   (`git fetch --prune` + fast-forward or conflict-checked 3-way merge
//!   against `<remote>/<branch>`) when the remote is reachable, and degrades
//!   cleanly via `OfflineFallback` when disconnected or when no remote is
//!   configured.
//! - Parallel multi-agent waves execute concurrently (`std::thread::scope`)
//!   with per-worker isolated Git worktrees, private `CARGO_TARGET_DIR`,
//!   private `SUSI_HOME`, disjoint 5-port socket stride
//!   (`MIN_ISOLATED_PORT_STRIDE`), recursive `Driver` -> `Engine`
//!   `CapabilityContract` admission, hardware-capacity admission, and
//!   serialized CAS integration onto `main`.

use crate::recursive::{
    CapabilityContract, ContainerRole, MIN_ISOLATED_PORT_STRIDE, RECURSIVE_CONTRACT_SCHEMA,
};
use crate::workflow::{
    normalize_scope, scopes_overlap, validate_dependencies, WorkflowSchedule, WorkflowTask,
    CLAIM_HEADER,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use susi_error::{EaiError, EaiResult};

const INTEGRATION_REF: &str = "refs/susi/integration/current";

/// Canonical lifecycle steps of the Susi native multi-agent workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStep {
    Start,
    Sync,
    Commit,
    Distill,
    Ship,
    Release,
}

impl WorkflowStep {
    /// Parse a workflow step name (`"step"` is accepted as an alias for `"sync"`).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "start" => Some(Self::Start),
            "sync" | "step" => Some(Self::Sync),
            "commit" => Some(Self::Commit),
            "distill" => Some(Self::Distill),
            "ship" => Some(Self::Ship),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Sync => "sync",
            Self::Commit => "commit",
            Self::Distill => "distill",
            Self::Ship => "ship",
            Self::Release => "release",
        }
    }
}

/// Configuration for pure-Git remote pull & merge reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteReconcileConfig {
    /// Git remote name (defaults to `"origin"`).
    pub remote_name: String,
    /// Remote development branch reconciled at every step (defaults to `"main"`).
    pub target_branch: String,
    /// When true (default), missing or unreachable remotes degrade cleanly to
    /// local state rather than aborting local autonomous execution.
    pub offline_fallback: bool,
}

impl Default for RemoteReconcileConfig {
    fn default() -> Self {
        Self {
            remote_name: "origin".to_owned(),
            target_branch: "main".to_owned(),
            offline_fallback: true,
        }
    }
}

/// Outcome of pure-Git remote pull & merge reconciliation on a workflow step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RemoteSyncMode {
    NoRemoteConfigured,
    OfflineFallback {
        reason: String,
    },
    UpToDate {
        head: String,
    },
    FastForwarded {
        from: String,
        to: String,
    },
    MergedThreeWay {
        local_head: String,
        remote_head: String,
        merged_commit: String,
    },
}

impl RemoteSyncMode {
    /// True when the remote was contacted and local lineage incorporates its tip.
    #[must_use]
    pub const fn reached_remote(&self) -> bool {
        match self {
            Self::NoRemoteConfigured | Self::OfflineFallback { .. } => false,
            Self::UpToDate { .. } | Self::FastForwarded { .. } | Self::MergedThreeWay { .. } => {
                true
            }
        }
    }
}

/// Receipt emitted by every workflow step (`start`, `sync`, `commit`, `distill`, `ship`, `release`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStepReceipt {
    pub step: WorkflowStep,
    pub agent: String,
    pub task_id: Option<String>,
    pub remote_sync: RemoteSyncMode,
    pub head_commit: String,
    pub receipt_sha256: String,
}

/// Isolated execution environment provisioned per parallel worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolatedWorkerWorkspace {
    pub agent: String,
    pub task_id: String,
    pub branch: String,
    pub base_commit: String,
    pub scopes: Vec<String>,
    pub worktree_dir: PathBuf,
    pub target_dir: PathBuf,
    pub susi_home_dir: PathBuf,
    pub port_offset: u16,
    pub driver_contract: CapabilityContract,
    pub engine_contract: CapabilityContract,
}

/// Task specification submitted to [`ParallelWorkflowHarness`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelAgentSpec {
    pub agent: String,
    pub task_id: String,
    pub title: String,
    pub deps: Vec<String>,
    pub scopes: Vec<String>,
}

impl WorkflowTask for ParallelAgentSpec {
    fn key(&self) -> &str {
        &self.agent
    }

    fn deps(&self) -> &[String] {
        &self.deps
    }

    fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

/// Distilled release snapshot summary emitted by `step_distill`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDistillSnapshot {
    pub release_name: String,
    pub branch_ref: String,
    pub base_commit: String,
    pub distilled_commit: String,
    pub included_tasks: Vec<String>,
    pub remote_sync: RemoteSyncMode,
    pub snapshot_sha256: String,
}

/// Complete execution report for a multi-wave parallel workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelWorkflowReport {
    pub waves_executed: usize,
    pub admitted_parallelism: usize,
    pub final_main_commit: String,
    pub landed_tasks: Vec<String>,
    pub step_receipts: Vec<WorkflowStepReceipt>,
    pub distilled_release: Option<WorkflowDistillSnapshot>,
}

/// Native parallel multi-agent workflow coordinator with remote pull & merge on every step.
pub struct ParallelWorkflowHarness {
    repo_root: PathBuf,
    isolation_root: PathBuf,
    remote: RemoteReconcileConfig,
    requested_parallelism: usize,
    base_port_offset: u16,
    integration_lock: Mutex<()>,
}

impl ParallelWorkflowHarness {
    /// Construct a workflow harness rooted at `repo_root` storing private worker
    /// worktrees, targets, and `SUSI_HOME` directories under `isolation_root`.
    pub fn new(
        repo_root: impl Into<PathBuf>,
        isolation_root: impl Into<PathBuf>,
        remote: RemoteReconcileConfig,
        requested_parallelism: usize,
    ) -> EaiResult<Self> {
        let repo_root = repo_root.into();
        let isolation_root = isolation_root.into();
        if !repo_root.exists() {
            return Err(EaiError::config(format!(
                "repository root does not exist: {}",
                repo_root.display()
            )));
        }
        fs::create_dir_all(isolation_root.join("worktrees"))
            .map_err(|e| EaiError::io(format!("create worktrees dir: {e}")))?;
        fs::create_dir_all(isolation_root.join("targets"))
            .map_err(|e| EaiError::io(format!("create targets dir: {e}")))?;
        fs::create_dir_all(isolation_root.join("homes"))
            .map_err(|e| EaiError::io(format!("create homes dir: {e}")))?;

        Ok(Self {
            repo_root,
            isolation_root,
            remote,
            requested_parallelism: requested_parallelism.max(1),
            base_port_offset: 100,
            integration_lock: Mutex::new(()),
        })
    }

    /// Capacity-admitted parallelism bounded by host logical cores.
    #[must_use]
    pub fn admitted_parallelism(&self) -> usize {
        let hw = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        self.requested_parallelism.clamp(1, hw)
    }

    /// Step 1 (`start`): pull & reconcile `<remote>/<branch>`, reserve scopes, and
    /// provision an isolated worktree + private `CARGO_TARGET_DIR` + private `SUSI_HOME`.
    pub fn step_start(
        &self,
        spec: &ParallelAgentSpec,
        slot_index: usize,
    ) -> EaiResult<(IsolatedWorkerWorkspace, WorkflowStepReceipt)> {
        validate_agent_name(&spec.agent)?;
        let normalized_scopes = normalize_spec_scopes(&spec.scopes)?;

        let remote_sync = {
            let _guard = self
                .integration_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            reconcile_repo_branch(&self.repo_root, &self.remote, false)?
        };

        let base_commit = resolve_start_base(&self.repo_root, &self.remote)?;
        let branch = format!("local-agent/{}", spec.agent);
        let worktree_dir = self.isolation_root.join("worktrees").join(&spec.agent);
        let target_dir = self.isolation_root.join("targets").join(&spec.agent);
        let susi_home_dir = self.isolation_root.join("homes").join(&spec.agent);

        fs::create_dir_all(&target_dir)
            .map_err(|e| EaiError::io(format!("create worker target dir: {e}")))?;
        fs::create_dir_all(&susi_home_dir)
            .map_err(|e| EaiError::io(format!("create worker SUSI_HOME dir: {e}")))?;

        {
            let _guard = self
                .integration_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            write_claim_file(
                &self.repo_root,
                &spec.agent,
                &worktree_dir,
                &base_commit,
                &normalized_scopes,
            )?;
            if worktree_dir.exists() {
                let _ = run_git(
                    &self.repo_root,
                    &[
                        "-c",
                        "core.hooksPath=",
                        "worktree",
                        "remove",
                        "--force",
                        path_str(&worktree_dir)?,
                    ],
                );
            }
            run_git(
                &self.repo_root,
                &[
                    "-c",
                    "core.hooksPath=",
                    "worktree",
                    "add",
                    "-B",
                    &branch,
                    path_str(&worktree_dir)?,
                    &base_commit,
                ],
            )?;
        }

        let slot_u16 = u16::try_from(slot_index.saturating_add(1)).unwrap_or(1);
        let port_offset = self
            .base_port_offset
            .saturating_add(slot_u16.saturating_mul(MIN_ISOLATED_PORT_STRIDE));

        let driver_contract = CapabilityContract {
            contract_version: RECURSIVE_CONTRACT_SCHEMA.to_owned(),
            component_id: "susi-harness-driver".to_owned(),
            role: ContainerRole::Driver,
            capabilities: ["wave_plan".to_owned(), "remote_reconcile".to_owned()]
                .into_iter()
                .collect(),
            verified_available: true,
        };
        driver_contract
            .validate()
            .map_err(|e| EaiError::governance(e.to_string()))?;

        let engine_contract = CapabilityContract {
            contract_version: RECURSIVE_CONTRACT_SCHEMA.to_owned(),
            component_id: format!("worker-{}", spec.agent),
            role: ContainerRole::Engine,
            capabilities: ["isolated_edit".to_owned(), "scoped_commit".to_owned()]
                .into_iter()
                .collect(),
            verified_available: true,
        };
        engine_contract
            .validate()
            .map_err(|e| EaiError::governance(e.to_string()))?;

        let ws = IsolatedWorkerWorkspace {
            agent: spec.agent.clone(),
            task_id: spec.task_id.clone(),
            branch,
            base_commit: base_commit.clone(),
            scopes: normalized_scopes,
            worktree_dir,
            target_dir,
            susi_home_dir,
            port_offset,
            driver_contract,
            engine_contract,
        };
        let receipt = build_step_receipt(
            WorkflowStep::Start,
            &spec.agent,
            Some(&spec.task_id),
            remote_sync,
            &base_commit,
        );
        Ok((ws, receipt))
    }

    /// Step 2 (`sync` / `step`): fetch `<remote>/<branch>` and fast-forward or
    /// cleanly 3-way merge upstream changes into the isolated worker worktree.
    pub fn step_sync(&self, ws: &mut IsolatedWorkerWorkspace) -> EaiResult<WorkflowStepReceipt> {
        let remote_sync = {
            let _guard = self
                .integration_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            reconcile_worktree_with_remote(&ws.worktree_dir, &self.remote)?
        };
        let head = run_git(&ws.worktree_dir, &["rev-parse", "HEAD"])?;
        if let RemoteSyncMode::FastForwarded { ref to, .. } = remote_sync {
            ws.base_commit = to.clone();
            let _guard = self
                .integration_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            write_claim_file(
                &self.repo_root,
                &ws.agent,
                &ws.worktree_dir,
                &ws.base_commit,
                &ws.scopes,
            )?;
        }
        Ok(build_step_receipt(
            WorkflowStep::Sync,
            &ws.agent,
            Some(&ws.task_id),
            remote_sync,
            &head,
        ))
    }

    /// Step 3 (`commit`): stage and commit scoped worker changes with `Task:` receipt,
    /// then reconcile (`pull` + FF/3-way merge) with `<remote>/<branch>`.
    pub fn step_commit(
        &self,
        ws: &mut IsolatedWorkerWorkspace,
        message: &str,
    ) -> EaiResult<WorkflowStepReceipt> {
        let changed = worktree_changed_paths(&ws.worktree_dir)?;
        if changed.is_empty() {
            return Err(EaiError::config(format!(
                "worker '{}' has no changes to commit",
                ws.agent
            )));
        }
        let task_manifest_rel = format!(".agents/tasks/{}.json", ws.task_id);
        for path in &changed {
            let norm = normalize_scope(path)?;
            let allowed = norm.eq_ignore_ascii_case(&task_manifest_rel)
                || ws.scopes.iter().any(|held| path_within_scope(held, &norm));
            if !allowed {
                return Err(EaiError::governance(format!(
                    "worker '{}' modified '{}' outside claimed scopes {:?}",
                    ws.agent, norm, ws.scopes
                )));
            }
        }

        run_git(&ws.worktree_dir, &["add", "-A", "--"])?;
        let full_msg = ensure_task_trailer(message, &ws.task_id);
        run_git(
            &ws.worktree_dir,
            &[
                "-c",
                "core.hooksPath=",
                "-c",
                "user.name=Susi Harness Worker",
                "-c",
                "user.email=susi-local@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                &full_msg,
            ],
        )?;

        let remote_sync = {
            let _guard = self
                .integration_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            reconcile_worktree_with_remote(&ws.worktree_dir, &self.remote)?
        };
        let head = run_git(&ws.worktree_dir, &["rev-parse", "HEAD"])?;
        Ok(build_step_receipt(
            WorkflowStep::Commit,
            &ws.agent,
            Some(&ws.task_id),
            remote_sync,
            &head,
        ))
    }

    /// Step 4 (`ship`): serialized integration step that fetches `<remote>/<branch>`,
    /// reconciles via FF or conflict-checked 3-way merge, updates local `main` &
    /// integration ref, and pushes fast-forward to `<remote>` when reachable.
    pub fn step_ship(&self, ws: &mut IsolatedWorkerWorkspace) -> EaiResult<WorkflowStepReceipt> {
        let _guard = self
            .integration_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if !worktree_changed_paths(&ws.worktree_dir)?.is_empty() {
            return Err(EaiError::governance(format!(
                "worker '{}' has uncommitted changes; commit before ship",
                ws.agent
            )));
        }

        // Pull and merge latest remote main into worker worktree first.
        let mut remote_sync = reconcile_worktree_with_remote(&ws.worktree_dir, &self.remote)?;
        let candidate_head = run_git(&ws.worktree_dir, &["rev-parse", "HEAD"])?;

        // Reconcile against local integration ref if another worker shipped while offline.
        if let Some(local_int) = read_optional_ref(&self.repo_root, INTEGRATION_REF)? {
            if !git_is_ancestor(&ws.worktree_dir, &local_int, &candidate_head)? {
                ensure_clean_merge_tree(&ws.worktree_dir, &candidate_head, &local_int)?;
                run_git(
                    &ws.worktree_dir,
                    &[
                        "-c",
                        "core.hooksPath=",
                        "-c",
                        "user.name=Susi Local Integration",
                        "-c",
                        "user.email=susi-local@localhost",
                        "-c",
                        "commit.gpgsign=false",
                        "merge",
                        "--no-ff",
                        "--no-edit",
                        &local_int,
                    ],
                )?;
            }
        }

        let shipped_head = run_git(&ws.worktree_dir, &["rev-parse", "HEAD"])?;

        // Attempt pure-Git fast-forward push to remote target branch if configured.
        if has_remote_configured(&self.repo_root, &self.remote.remote_name)? {
            let refspec = format!("{shipped_head}:refs/heads/{}", self.remote.target_branch);
            let push_res = run_git(
                &self.repo_root,
                &[
                    "push",
                    "--porcelain",
                    "--no-follow-tags",
                    "--",
                    &self.remote.remote_name,
                    &refspec,
                ],
            );
            match push_res {
                Ok(_) => {
                    let remote_tracking = format!(
                        "refs/remotes/{}/{}",
                        self.remote.remote_name, self.remote.target_branch
                    );
                    let _ = run_git(
                        &self.repo_root,
                        &["update-ref", &remote_tracking, &shipped_head],
                    );
                }
                Err(err) if self.remote.offline_fallback => {
                    remote_sync = RemoteSyncMode::OfflineFallback {
                        reason: format!("remote push deferred offline: {err}"),
                    };
                }
                Err(err) => return Err(err),
            }
        }

        run_git(
            &self.repo_root,
            &["update-ref", INTEGRATION_REF, &shipped_head],
        )?;
        // Advance primary `refs/heads/<target_branch>` if primary checkout is not dirty on it.
        advance_local_main_if_safe(&self.repo_root, &self.remote.target_branch, &shipped_head)?;

        Ok(build_step_receipt(
            WorkflowStep::Ship,
            &ws.agent,
            Some(&ws.task_id),
            remote_sync,
            &shipped_head,
        ))
    }

    /// Step 5 (`release`): pull & reconcile `<remote>/<branch>`, verify worker head
    /// has landed, release scope claim, and remove scratch worktree cleanly.
    pub fn step_release(&self, ws: &IsolatedWorkerWorkspace) -> EaiResult<WorkflowStepReceipt> {
        let _guard = self
            .integration_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let remote_sync = reconcile_repo_branch(&self.repo_root, &self.remote, false)?;
        let worker_head = run_git(&ws.worktree_dir, &["rev-parse", "HEAD"])?;

        let landed_int = match read_optional_ref(&self.repo_root, INTEGRATION_REF)? {
            Some(int_oid) => git_is_ancestor(&self.repo_root, &worker_head, &int_oid)?,
            None => false,
        };
        let remote_ref = format!(
            "refs/remotes/{}/{}",
            self.remote.remote_name, self.remote.target_branch
        );
        let landed_remote = match read_optional_ref(&self.repo_root, &remote_ref)? {
            Some(rem_oid) => git_is_ancestor(&self.repo_root, &worker_head, &rem_oid)?,
            None => false,
        };

        if !landed_int && !landed_remote && worker_head != ws.base_commit {
            return Err(EaiError::governance(format!(
                "refusing to release worker '{}': commit {worker_head} has not landed on integration or remote {}",
                ws.agent, self.remote.target_branch
            )));
        }

        remove_claim_file(&self.repo_root, &ws.agent)?;
        if ws.worktree_dir.exists() {
            let _ = run_git(
                &self.repo_root,
                &[
                    "-c",
                    "core.hooksPath=",
                    "worktree",
                    "remove",
                    "--force",
                    path_str(&ws.worktree_dir)?,
                ],
            );
        }
        Ok(build_step_receipt(
            WorkflowStep::Release,
            &ws.agent,
            Some(&ws.task_id),
            remote_sync,
            &worker_head,
        ))
    }

    /// Step 6 (`distill`): pull & reconcile `<remote>/<branch>`, extract `Task:`
    /// receipts in `base_rev..head`, and materialize a deterministic `refs/heads/release/<name>`
    /// branch + SHA-256 snapshot record.
    pub fn step_distill(
        &self,
        release_name: &str,
        base_rev: &str,
    ) -> EaiResult<(WorkflowDistillSnapshot, WorkflowStepReceipt)> {
        validate_agent_name(release_name)?;
        let _guard = self
            .integration_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let remote_sync = reconcile_repo_branch(&self.repo_root, &self.remote, false)?;
        let base_commit = run_git(
            &self.repo_root,
            &["rev-parse", &format!("{base_rev}^{{commit}}")],
        )?;
        let head_commit = resolve_start_base(&self.repo_root, &self.remote)?;

        let log_out = run_git(
            &self.repo_root,
            &[
                "log",
                "--reverse",
                "--no-merges",
                "--format=%H%x1f%(trailers:key=Task,valueonly,separator=%x1e)",
                &format!("{base_commit}..{head_commit}"),
            ],
        )?;
        let mut included_tasks = Vec::new();
        let mut seen = BTreeSet::new();
        for line in log_out.lines().filter(|l| !l.trim().is_empty()) {
            let (_oid, trailers) = line.split_once('\u{1f}').unwrap_or((line, ""));
            for t in trailers
                .split('\u{1e}')
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                if seen.insert(t.to_owned()) {
                    included_tasks.push(t.to_owned());
                }
            }
        }

        let branch_ref = format!("refs/heads/release/{release_name}");
        run_git(&self.repo_root, &["update-ref", &branch_ref, &head_commit])?;

        let payload = serde_json::json!({
            "schema": "susi-workflow-distill-snapshot/v1",
            "release_name": release_name,
            "branch_ref": branch_ref,
            "base_commit": base_commit,
            "distilled_commit": head_commit,
            "included_tasks": included_tasks,
        });
        let snapshot_sha256 = sha256_json(&payload);
        let snapshot = WorkflowDistillSnapshot {
            release_name: release_name.to_owned(),
            branch_ref,
            base_commit,
            distilled_commit: head_commit.clone(),
            included_tasks,
            remote_sync: remote_sync.clone(),
            snapshot_sha256,
        };
        let receipt = build_step_receipt(
            WorkflowStep::Distill,
            "distiller",
            None,
            remote_sync,
            &head_commit,
        );
        Ok((snapshot, receipt))
    }

    /// Execute a dependency- and scope-validated multi-agent plan across parallel
    /// waves, running worker closures concurrently in isolated workspaces and
    /// reconciling with `<remote>/<branch>` at every step.
    pub fn execute_parallel_plan<F>(
        &self,
        specs: &[ParallelAgentSpec],
        distill_release_name: Option<&str>,
        worker_fn: F,
    ) -> EaiResult<ParallelWorkflowReport>
    where
        F: Fn(&IsolatedWorkerWorkspace) -> EaiResult<String> + Sync,
    {
        validate_dependencies(specs).map_err(|e| EaiError::config(e.to_string()))?;
        let initial_base = resolve_start_base(&self.repo_root, &self.remote)?;
        let admitted_parallelism = self.admitted_parallelism();
        let schedule = WorkflowSchedule::build(specs, &BTreeSet::new(), &[], admitted_parallelism)
            .map_err(|e| EaiError::config(e.to_string()))?;

        let mut step_receipts = Vec::new();
        let mut landed_tasks = Vec::new();
        let mut slot_counter = 0usize;

        for wave in &schedule.waves {
            let mut wave_workspaces = Vec::with_capacity(wave.tasks.len());
            for spec in &wave.tasks {
                let (ws, start_rcpt) = self.step_start(spec, slot_counter)?;
                slot_counter = slot_counter.saturating_add(1);
                step_receipts.push(start_rcpt);
                wave_workspaces.push((*spec, ws));
            }

            // Execute worker callbacks concurrently in isolated worktrees/targets/homes!
            let parallel_outcomes: Vec<EaiResult<String>> = std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(wave_workspaces.len());
                for (_spec, ws) in &wave_workspaces {
                    let worker_ref = &worker_fn;
                    handles.push(scope.spawn(move || worker_ref(ws)));
                }
                let mut results = Vec::with_capacity(handles.len());
                for handle in handles {
                    match handle.join() {
                        Ok(res) => results.push(res),
                        Err(_) => results.push(Err(EaiError::governance(
                            "parallel workflow worker thread terminated unexpectedly",
                        ))),
                    }
                }
                results
            });

            let mut commit_messages = Vec::with_capacity(parallel_outcomes.len());
            for outcome in parallel_outcomes {
                commit_messages.push(outcome?);
            }

            // Commit scoped edits, sync remote, ship, and release each worker in deterministic wave order.
            for ((spec, mut ws), msg) in wave_workspaces.into_iter().zip(commit_messages) {
                let commit_rcpt = self.step_commit(&mut ws, &msg)?;
                step_receipts.push(commit_rcpt);

                let sync_rcpt = self.step_sync(&mut ws)?;
                step_receipts.push(sync_rcpt);

                let ship_rcpt = self.step_ship(&mut ws)?;
                step_receipts.push(ship_rcpt);

                let release_rcpt = self.step_release(&ws)?;
                step_receipts.push(release_rcpt);

                landed_tasks.push(spec.task_id.clone());
            }
        }

        let distilled_release = match distill_release_name {
            Some(rel_name) => {
                let (snap, dist_rcpt) = self.step_distill(rel_name, &initial_base)?;
                step_receipts.push(dist_rcpt);
                Some(snap)
            }
            None => None,
        };

        let final_main_commit = resolve_start_base(&self.repo_root, &self.remote)?;
        Ok(ParallelWorkflowReport {
            waves_executed: schedule.waves.len(),
            admitted_parallelism,
            final_main_commit,
            landed_tasks,
            step_receipts,
            distilled_release,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure-Git Remote Pull & Merge Primitives
// ---------------------------------------------------------------------------

/// Reconcile a checked-out worktree (`worktree_dir`) with `<remote>/<target_branch>`
/// via `git fetch` + fast-forward or conflict-checked 3-way merge.
pub fn reconcile_worktree_with_remote(
    worktree_dir: &Path,
    cfg: &RemoteReconcileConfig,
) -> EaiResult<RemoteSyncMode> {
    reconcile_repo_branch(worktree_dir, cfg, true)
}

fn reconcile_repo_branch(
    dir: &Path,
    cfg: &RemoteReconcileConfig,
    merge_working_tree: bool,
) -> EaiResult<RemoteSyncMode> {
    if !has_remote_configured(dir, &cfg.remote_name)? {
        if cfg.offline_fallback {
            return Ok(RemoteSyncMode::NoRemoteConfigured);
        }
        return Err(EaiError::config(format!(
            "remote '{}' is not configured",
            cfg.remote_name
        )));
    }

    let remote_tracking = format!("refs/remotes/{}/{}", cfg.remote_name, cfg.target_branch);
    let fetch_refspec = format!("+refs/heads/{}:{remote_tracking}", cfg.target_branch);
    let fetch_res = run_git(
        dir,
        &[
            "fetch",
            "--prune",
            "--no-tags",
            "--",
            &cfg.remote_name,
            &fetch_refspec,
        ],
    );
    if let Err(err) = fetch_res {
        if cfg.offline_fallback {
            return Ok(RemoteSyncMode::OfflineFallback {
                reason: err.to_string(),
            });
        }
        return Err(err);
    }

    let Some(remote_oid) = read_optional_ref(dir, &remote_tracking)? else {
        let head = run_git(dir, &["rev-parse", "HEAD"])?;
        return Ok(RemoteSyncMode::UpToDate { head });
    };

    let local_oid = run_git(dir, &["rev-parse", "HEAD"])?;
    if local_oid == remote_oid || git_is_ancestor(dir, &remote_oid, &local_oid)? {
        return Ok(RemoteSyncMode::UpToDate { head: local_oid });
    }

    if !merge_working_tree {
        // Primary repository: advance local tracking/integration base safely without
        // clobbering uncommitted primary worktree files if any exist.
        advance_local_main_if_safe(dir, &cfg.target_branch, &remote_oid)?;
        if git_is_ancestor(dir, &local_oid, &remote_oid)? {
            return Ok(RemoteSyncMode::FastForwarded {
                from: local_oid,
                to: remote_oid,
            });
        }
        return Ok(RemoteSyncMode::UpToDate { head: remote_oid });
    }

    if git_is_ancestor(dir, &local_oid, &remote_oid)? {
        if worktree_changed_paths(dir)?.is_empty() {
            run_git(dir, &["merge", "--ff-only", &remote_oid])?;
            return Ok(RemoteSyncMode::FastForwarded {
                from: local_oid,
                to: remote_oid,
            });
        }
        return Err(EaiError::governance(
            "cannot fast-forward dirty worktree; commit or stash local changes first",
        ));
    }

    if !worktree_changed_paths(dir)?.is_empty() {
        return Err(EaiError::governance(
            "cannot 3-way merge remote updates into dirty worktree; commit scoped changes first",
        ));
    }

    ensure_clean_merge_tree(dir, &local_oid, &remote_oid)?;
    run_git(
        dir,
        &[
            "-c",
            "core.hooksPath=",
            "-c",
            "user.name=Susi Workflow Reconciler",
            "-c",
            "user.email=susi-local@localhost",
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-ff",
            "--no-edit",
            &remote_oid,
        ],
    )?;
    let merged_commit = run_git(dir, &["rev-parse", "HEAD"])?;
    Ok(RemoteSyncMode::MergedThreeWay {
        local_head: local_oid,
        remote_head: remote_oid,
        merged_commit,
    })
}

fn ensure_clean_merge_tree(dir: &Path, left: &str, right: &str) -> EaiResult<String> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(["merge-tree", "--write-tree", left, right])
        .output()
        .map_err(|e| EaiError::io(format!("git merge-tree failed to spawn: {e}")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stdout);
        return Err(EaiError::governance(format!(
            "remote merge conflict detected between {left} and {right}: {}",
            detail.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned())
}

fn advance_local_main_if_safe(root: &Path, branch: &str, new_oid: &str) -> EaiResult<()> {
    let branch_ref = format!("refs/heads/{branch}");
    let current_branch = run_git(root, &["branch", "--show-current"]).unwrap_or_default();
    if current_branch == branch {
        if worktree_changed_paths(root)?.is_empty() {
            if let Ok(cur) = run_git(root, &["rev-parse", "HEAD"]) {
                if cur != new_oid && git_is_ancestor(root, &cur, new_oid)? {
                    let _ = run_git(root, &["merge", "--ff-only", new_oid]);
                }
            }
        }
    } else if let Some(cur) = read_optional_ref(root, &branch_ref)? {
        if cur != new_oid && git_is_ancestor(root, &cur, new_oid)? {
            let _ = run_git(root, &["update-ref", &branch_ref, new_oid, &cur]);
        }
    }
    Ok(())
}

fn resolve_start_base(root: &Path, cfg: &RemoteReconcileConfig) -> EaiResult<String> {
    let remote_ref = format!("refs/remotes/{}/{}", cfg.remote_name, cfg.target_branch);
    let integrated = read_optional_ref(root, INTEGRATION_REF)?;
    let fetched = read_optional_ref(root, &remote_ref)?;
    match (integrated, fetched) {
        (Some(i), Some(f)) => {
            if i != f && git_is_ancestor(root, &i, &f)? {
                Ok(f)
            } else {
                Ok(i)
            }
        }
        (Some(i), None) => Ok(i),
        (None, Some(f)) => Ok(f),
        (None, None) => run_git(root, &["rev-parse", "HEAD"]),
    }
}

fn has_remote_configured(dir: &Path, remote_name: &str) -> EaiResult<bool> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(["remote", "get-url", remote_name])
        .output()
        .map_err(|e| EaiError::io(format!("git remote get-url: {e}")))?;
    Ok(out.status.success())
}

fn read_optional_ref(dir: &Path, reference: &str) -> EaiResult<Option<String>> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .map_err(|e| EaiError::io(format!("git rev-parse {reference}: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if oid.is_empty() {
        Ok(None)
    } else {
        Ok(Some(oid))
    }
}

fn git_is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> EaiResult<bool> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(|e| EaiError::io(format!("git merge-base: {e}")))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(EaiError::io(format!(
            "git merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

fn worktree_changed_paths(dir: &Path) -> EaiResult<Vec<String>> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .map_err(|e| EaiError::io(format!("git status: {e}")))?;
    if !out.status.success() {
        return Err(EaiError::io("git status failed"));
    }
    let mut paths = Vec::new();
    let mut iter = out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let Some(entry) = iter.next() {
        if entry.len() < 4 {
            continue;
        }
        let status = &entry[..2];
        let rel = String::from_utf8_lossy(&entry[3..]).into_owned();
        paths.push(rel);
        if status.contains(&b'R') || status.contains(&b'C') {
            if let Some(orig) = iter.next() {
                paths.push(String::from_utf8_lossy(orig).into_owned());
            }
        }
    }
    Ok(paths)
}

fn run_git(dir: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new("git")
        .arg("--literal-pathspecs")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| EaiError::io(format!("git {:?} spawn error: {e}", args.first())))?;
    if !out.status.success() {
        return Err(EaiError::io(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn write_claim_file(
    root: &Path,
    agent: &str,
    worktree: &Path,
    base: &str,
    scopes: &[String],
) -> EaiResult<()> {
    let common = PathBuf::from(run_git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let claims_dir = common.join("susi-local-workflow").join("claims");
    fs::create_dir_all(&claims_dir).map_err(|e| EaiError::io(format!("create claims dir: {e}")))?;
    for entry in fs::read_dir(&claims_dir).map_err(|e| EaiError::io(e.to_string()))? {
        let entry = entry.map_err(|e| EaiError::io(e.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("claim") {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|e| EaiError::io(e.to_string()))?;
        let mut lines = content.lines();
        if lines.next() != Some(CLAIM_HEADER) {
            continue;
        }
        let Some(other_agent) = lines.next() else {
            continue;
        };
        if other_agent == agent {
            continue;
        }
        let _wt = lines.next();
        let _base = lines.next();
        for other_scope in lines {
            for my_scope in scopes {
                if scopes_overlap(other_scope, my_scope) {
                    return Err(EaiError::governance(format!(
                        "scope '{my_scope}' for '{agent}' overlaps active claim by '{other_agent}' on '{other_scope}'"
                    )));
                }
            }
        }
    }
    let body = format!(
        "{CLAIM_HEADER}\n{agent}\n{}\n{base}\n{}\n",
        worktree.display(),
        scopes.join("\n")
    );
    fs::write(claims_dir.join(format!("{agent}.claim")), body)
        .map_err(|e| EaiError::io(format!("write claim file: {e}")))
}

fn remove_claim_file(root: &Path, agent: &str) -> EaiResult<()> {
    let common = PathBuf::from(run_git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let claim_path = common
        .join("susi-local-workflow")
        .join("claims")
        .join(format!("{agent}.claim"));
    if claim_path.exists() {
        fs::remove_file(claim_path).map_err(|e| EaiError::io(format!("remove claim: {e}")))?;
    }
    Ok(())
}

fn validate_agent_name(name: &str) -> EaiResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !ok {
        return Err(EaiError::config(format!(
            "invalid workflow identifier '{name}'; use lowercase ascii, digits, and hyphens"
        )));
    }
    Ok(())
}

fn normalize_spec_scopes(scopes: &[String]) -> EaiResult<Vec<String>> {
    if scopes.is_empty() {
        return Err(EaiError::config("worker requires at least one scope"));
    }
    let mut out = Vec::with_capacity(scopes.len());
    for raw in scopes {
        let norm = normalize_scope(raw)?;
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    Ok(out)
}

fn path_within_scope(held_scope: &str, candidate: &str) -> bool {
    candidate.eq_ignore_ascii_case(held_scope)
        || (candidate.len() > held_scope.len()
            && candidate.as_bytes().get(held_scope.len()) == Some(&b'/')
            && candidate[..held_scope.len()].eq_ignore_ascii_case(held_scope))
}

fn ensure_task_trailer(message: &str, task_id: &str) -> String {
    let needle = format!("Task: {task_id}");
    if message.lines().any(|line| line.trim() == needle) {
        message.trim_end().to_owned()
    } else {
        format!("{}\n\nTask: {task_id}", message.trim_end())
    }
}

fn path_str(path: &Path) -> EaiResult<&str> {
    path.to_str()
        .ok_or_else(|| EaiError::config("non-UTF-8 filesystem path"))
}

fn build_step_receipt(
    step: WorkflowStep,
    agent: &str,
    task_id: Option<&str>,
    remote_sync: RemoteSyncMode,
    head_commit: &str,
) -> WorkflowStepReceipt {
    let payload = serde_json::json!({
        "schema": "susi-workflow-step-receipt/v1",
        "step": step.as_str(),
        "agent": agent,
        "task_id": task_id,
        "remote_sync": &remote_sync,
        "head_commit": head_commit,
    });
    let receipt_sha256 = sha256_json(&payload);
    WorkflowStepReceipt {
        step,
        agent: agent.to_owned(),
        task_id: task_id.map(str::to_owned),
        remote_sync,
        head_commit: head_commit.to_owned(),
        receipt_sha256,
    }
}

fn sha256_json(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
#[path = "tests/workflow_runner.rs"]
mod tests;
