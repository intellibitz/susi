// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use crate::cancel_propagate::{CancelBus, CancelToken, Descendant, WorkerKind};
use crate::fair_queue::{EnqueueResult, FairQueue, QueueLimits, QueuedMission};
use crate::independent_verify::{verification_satisfied, ReviewConclusion};
use crate::joint_consensus::{overlapping_disjoint_blocked, Electorate, MembershipTransition};
use crate::mission_persist::{NodeTerminal, PersistedMission, PersistedNode};
use crate::mission_resume::{DagNodeView, MissionView, NodeView};
use crate::node_enrollment::{enroll, Enrollment};
use crate::resource_schedule::{admit, reserve, Admit, DagNode as ResNode, Resources};
use crate::role_select::{select_roles, AgentEvidence, RoleAssignment, SelectError};
use crate::side_effects::{reconcile_dispatch_side_effect, ActionOutcome};
use crate::susi_core::evidence::EvidenceRecord;
use crate::susi_error::{EaiError, EaiResult};
use crate::task_lease::{CompleteVerdict, LeaseTable, TaskLease};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use susi_gawd_agents::agents::{instantiate_native_agent, MissionBlackboard};

#[derive(Debug, Clone)]
pub struct TaskNode {
    pub task_id: usize,
    pub title: String,
    pub goal: String,
    pub dependencies: Vec<usize>,
    pub assigned_agent: Option<String>,
    pub completed: bool,
}

pub struct MissionDag {
    pub nodes: Vec<TaskNode>,
    /// Per-dispatch ownership fences (VC-201-022). Completions without the
    /// live fence are refused so a recovered worker cannot publish.
    pub leases: LeaseTable,
    /// Side-effect outcomes keyed by persist id (VC-201-023).
    pub side_effects: std::collections::BTreeMap<String, ActionOutcome>,
    /// Cancellation bus for workers/peers (VC-201-026).
    pub cancel: CancelBus,
    /// Fair multi-mission admit queue (VC-201-025).
    pub fair_queue: FairQueue,
    /// Active swarm roster; changes commit only under joint consensus (VC-201-032).
    pub roster: Electorate,
    /// In-flight membership transition awaiting joint quorum.
    pub pending_membership: Option<MembershipTransition>,
}

pub type SwarmDag = MissionDag;

/// Borrowed mission persistence handle passed through DAG execution.
pub struct MissionPersistCtx<'a> {
    pub mission: &'a mut PersistedMission,
    pub dir: &'a Path,
}

/// One node's run: index, output, elapsed ms, receipt args, lease owner,
/// fence, the worker's private write scope (folded before results are
/// processed), and whether the worker aborted on mission cancellation
/// (T-DEVIN-8) — cancelled runs are discarded without fold-back.
struct NodeRun {
    idx: usize,
    res: EaiResult<String>,
    elapsed_ms: u64,
    calls: Vec<String>,
    owner: String,
    fence: u64,
    scope: crate::writer_isolation::WorkerScope,
    cancelled: bool,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unique-per-process execution counter minting task-registry cancel scopes
/// for each DAG batch run (T-DEVIN-8).
static DAG_EXEC_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl MissionDag {
    pub fn new(initial_goal: &str) -> Self {
        Self {
            nodes: vec![TaskNode {
                task_id: 0,
                title: "Primary Mission Analysis".to_string(),
                goal: initial_goal.to_string(),
                dependencies: vec![],
                assigned_agent: None,
                completed: false,
            }],
            leases: LeaseTable::new(),
            side_effects: std::collections::BTreeMap::new(),
            cancel: CancelBus::default(),
            fair_queue: FairQueue::new(QueueLimits::default()),
            roster: Electorate(BTreeSet::new()),
            pending_membership: None,
        }
    }

    /// Stable persisted node id for a DAG index (`n0`, `n1`, …).
    #[must_use]
    pub fn persist_id(idx: usize) -> String {
        format!("n{idx}")
    }

    /// Append a dependent task node; returns its index.
    pub fn push_node(
        &mut self,
        title: impl Into<String>,
        goal: impl Into<String>,
        deps: Vec<usize>,
    ) -> usize {
        let task_id = self.nodes.len();
        self.nodes.push(TaskNode {
            task_id,
            title: title.into(),
            goal: goal.into(),
            dependencies: deps,
            assigned_agent: None,
            completed: false,
        });
        task_id
    }

    /// Snapshot live DAG state into a [`PersistedMission`] (VC-201-021 wiring).
    #[must_use]
    pub fn to_persisted(&self, mission_id: &str) -> PersistedMission {
        let mut mission = PersistedMission::new(mission_id);
        for (idx, node) in self.nodes.iter().enumerate() {
            let id = Self::persist_id(idx);
            let state = if node.completed {
                NodeTerminal::Completed
            } else {
                NodeTerminal::Pending
            };
            mission.upsert_node(PersistedNode {
                id,
                input: node.goal.clone(),
                dependencies: node
                    .dependencies
                    .iter()
                    .map(|&d| Self::persist_id(d))
                    .collect(),
                output: None,
                state,
            });
        }
        mission
    }

    /// Rebuild a DAG from persisted state. Completed nodes stay completed so
    /// resume skips them; unfinished nodes stay pending/runnable.
    #[must_use]
    pub fn from_persisted(mission: &PersistedMission) -> Self {
        // Ordered by numeric suffix of n<idx> when parseable, else by id.
        let mut entries: Vec<&PersistedNode> = mission.nodes.values().collect();
        entries.sort_by_key(|n| {
            n.id.strip_prefix('n')
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(usize::MAX)
        });
        let id_to_idx: std::collections::BTreeMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let nodes = entries
            .iter()
            .enumerate()
            .map(|(idx, n)| TaskNode {
                task_id: idx,
                title: n.id.clone(),
                goal: n.input.clone(),
                dependencies: n
                    .dependencies
                    .iter()
                    .filter_map(|d| id_to_idx.get(d.as_str()).copied())
                    .collect(),
                assigned_agent: None,
                completed: n.state == NodeTerminal::Completed,
            })
            .collect();
        Self {
            nodes,
            leases: LeaseTable::new(),
            side_effects: std::collections::BTreeMap::new(),
            cancel: CancelBus::default(),
            fair_queue: FairQueue::new(QueueLimits::default()),
            roster: Electorate(BTreeSet::new()),
            pending_membership: None,
        }
    }

    /// Record / reconcile a tool side effect for a DAG node (dispatch wiring).
    pub fn record_side_effect(
        &mut self,
        idx: usize,
        tool: &str,
        prior: Option<&ActionOutcome>,
    ) -> ActionOutcome {
        let outcome = reconcile_dispatch_side_effect(tool, prior);
        self.side_effects
            .insert(Self::persist_id(idx), outcome.clone());
        outcome
    }

    /// Whether a crashed node may retry its last recorded side effect.
    #[must_use]
    pub fn may_retry_node(&self, idx: usize) -> bool {
        self.side_effects
            .get(&Self::persist_id(idx))
            .is_none_or(|o| crate::side_effects::may_retry_after_crash(o.class))
    }

    /// Attach a cancel token and register a worker descendant for this node.
    pub fn register_cancel_worker(
        &mut self,
        idx: usize,
        kind: WorkerKind,
        cancellable: bool,
        deadline_unix: u64,
    ) {
        self.register_cancel_worker_scoped(
            idx,
            deadline_unix,
            Descendant {
                id: String::new(),
                kind,
                cancellable,
                cancel_scope: None,
                signal: None,
            },
        );
    }

    /// As [`Self::register_cancel_worker`], with the descendant's real
    /// termination handles set on `worker`: a task-registry `cancel_scope`
    /// (running commands under it are killed on propagate) and a shared
    /// `signal` the worker polls between steps (T-DEVIN-8). The worker's
    /// `id` is overwritten with the node's persist id.
    pub fn register_cancel_worker_scoped(
        &mut self,
        idx: usize,
        deadline_unix: u64,
        mut worker: Descendant,
    ) {
        worker.id = Self::persist_id(idx);
        if self.cancel.token.is_none() {
            self.cancel
                .set_token(CancelToken::fresh(&worker.id, deadline_unix));
        }
        self.cancel.register(worker);
    }

    /// Propagate cancellation through registered workers/peers.
    pub fn cancel_propagate(&mut self) -> Vec<String> {
        self.cancel.propagate()
    }

    /// True when the mission cancel token is cancelled or past deadline.
    #[must_use]
    pub fn is_cancelled(&self, now: u64) -> bool {
        match &self.cancel.token {
            Some(t) => t.cancelled || t.remaining_secs(now).is_none(),
            None => false,
        }
    }

    /// Admit this mission into the fair concurrent queue (VC-201-025).
    pub fn fair_admit(
        &mut self,
        mission_id: &str,
        workspace: &str,
        now: u64,
        weight: u32,
    ) -> EnqueueResult {
        self.fair_queue.enqueue(QueuedMission {
            mission_id: mission_id.to_string(),
            workspace: workspace.to_string(),
            enqueued_at: now,
            weight,
        })
    }

    /// Release a finished mission and promote the next fair waiter.
    pub fn fair_complete(&mut self, mission_id: &str, now: u64) -> Option<QueuedMission> {
        self.fair_queue.complete(mission_id, now)
    }

    /// Select implementer/verifier (and optional specialist) from the native
    /// fleet only — DynamicAgent phantoms without a native factory are dropped
    /// before [`select_roles`] runs (VC-201-027).
    pub fn assign_native_roles(
        &mut self,
        fleet: &[AgentEvidence],
        required_caps: &BTreeSet<String>,
        solo_cost: f64,
    ) -> Result<RoleAssignment, SelectError> {
        let native: Vec<AgentEvidence> = fleet
            .iter()
            .filter(|a| instantiate_native_agent(&a.id).is_some())
            .cloned()
            .collect();
        let assignment = select_roles(&native, required_caps, solo_cost)?;

        if let Some(root) = self.nodes.first_mut() {
            root.assigned_agent = Some(assignment.implementer.clone());
        }

        let verify_idx = self
            .nodes
            .iter()
            .position(|n| n.title == "Independent Verify")
            .unwrap_or_else(|| {
                self.push_node(
                    "Independent Verify",
                    "independently verify implementer output",
                    vec![0],
                )
            });
        if let Some(node) = self.nodes.get_mut(verify_idx) {
            node.assigned_agent = Some(assignment.verifier.clone());
        }

        if let Some(specialist) = assignment.specialist.as_ref() {
            let spec_idx = self
                .nodes
                .iter()
                .position(|n| n.title == "Specialist")
                .unwrap_or_else(|| {
                    self.push_node("Specialist", "cover remaining capability gap", vec![0])
                });
            if let Some(node) = self.nodes.get_mut(spec_idx) {
                node.assigned_agent = Some(specialist.clone());
            }
        }

        Ok(assignment)
    }

    /// Accept a swarm verify-path conclusion only when independent evidence
    /// satisfies VC-201-028: reviewer ≠ implementer, pass, unique receipts.
    /// On success marks the Independent Verify node complete.
    pub fn accept_independent_verify(&mut self, conclusion: &ReviewConclusion) -> bool {
        let implementer = self
            .nodes
            .first()
            .and_then(|n| n.assigned_agent.as_deref())
            .unwrap_or(conclusion.implementer.as_str());
        if !verification_satisfied(conclusion, implementer) {
            return false;
        }
        let verify_idx = self
            .nodes
            .iter()
            .position(|n| n.title == "Independent Verify");
        if let Some(idx) = verify_idx {
            if let Some(node) = self.nodes.get_mut(idx) {
                if node
                    .assigned_agent
                    .as_deref()
                    .is_some_and(|a| a != conclusion.reviewer)
                {
                    // Assigned verifier must match the conclusion reviewer.
                    return false;
                }
                node.completed = true;
            }
        }
        true
    }

    /// Build a durable resume CLI view from live DAG + optional side-effect
    /// uncertainty (VC-201-030). Partial outcomes never report full success.
    #[must_use]
    pub fn resume_cli_view(&self, mission_id: &str) -> MissionView {
        let mut view = MissionView::new(mission_id);
        for (idx, node) in self.nodes.iter().enumerate() {
            let id = Self::persist_id(idx);
            let uncertain = self
                .side_effects
                .get(&id)
                .is_some_and(|o| o.uncertain_external);
            let cancelled = self.cancel.descendants.contains_key(&id)
                && self.cancel.token.as_ref().is_some_and(|t| t.cancelled);
            let (node_view, resumable) = if node.completed {
                (NodeView::Completed, false)
            } else if cancelled {
                (NodeView::Cancelled, false)
            } else if uncertain {
                (NodeView::Uncertain, true)
            } else if node
                .dependencies
                .iter()
                .all(|&d| self.nodes.get(d).is_some_and(|n| n.completed))
            {
                (NodeView::Running, true)
            } else {
                (NodeView::Blocked, true)
            };
            view.upsert(DagNodeView {
                id,
                view: node_view,
                resumable,
                output: None,
            });
        }
        view
    }

    /// Resume CLI status text for a mission view — never claims full success
    /// when work remains (VC-201-030).
    #[must_use]
    pub fn resume_cli_status(view: &MissionView) -> String {
        crate::mission_resume::cli_status_line(view)
    }

    /// Seed the active roster (bootstrap only — subsequent changes go through
    /// joint-consensus propose/endorse/commit).
    pub fn bootstrap_roster(&mut self, members: impl IntoIterator<Item = impl Into<String>>) {
        self.roster = Electorate::from_ids(members);
        self.pending_membership = None;
    }

    /// Propose a roster change; replaces any prior unfinished transition.
    pub fn propose_membership(&mut self, new: Electorate) {
        self.pending_membership = Some(MembershipTransition::new(self.roster.clone(), new));
    }

    /// Record an endorsement on the pending transition (ignored if none).
    pub fn endorse_membership(&mut self, member: &str) {
        if let Some(t) = self.pending_membership.as_mut() {
            t.endorse(member);
        }
    }

    /// Commit the pending roster change only under joint quorum. Returns
    /// whether the roster advanced. Concurrent overlapping transitions that
    /// would leave disjoint active rosters are refused via
    /// [`overlapping_disjoint_blocked`].
    pub fn try_commit_membership(&mut self, competing: Option<&MembershipTransition>) -> bool {
        let Some(pending) = self.pending_membership.as_ref() else {
            return false;
        };
        if !pending.can_commit() {
            return false;
        }
        if let Some(other) = competing {
            if !overlapping_disjoint_blocked(pending, other) {
                return false;
            }
        }
        let new_roster = pending.new.clone();
        self.roster = new_roster;
        self.pending_membership = None;
        true
    }

    /// Production node enrollment: join token + mTLS provisions peer id into
    /// the live roster at the given cluster key epoch.
    pub fn enroll_node(
        &mut self,
        token: &str,
        expected_token: &str,
        mtls_identity: Option<&str>,
        key_epoch: u64,
    ) -> Enrollment {
        let outcome = enroll(token, expected_token, mtls_identity, key_epoch);
        if outcome.accepted {
            if let Some(peer) = outcome.peer_id.as_ref() {
                self.roster.0.insert(peer.clone());
            }
        }
        outcome
    }

    /// Default resource request for a DAG node (dispatch scheduling).
    #[must_use]
    pub fn default_resource_node(idx: usize) -> ResNode {
        ResNode {
            id: Self::persist_id(idx),
            cpu: 1.0,
            gpu_mem_gb: 0.0,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        }
    }

    /// Filter ready indices by live resource constraints; reserves as admits
    /// succeed so concurrent admits never oversubscribe (VC-201-024).
    pub fn schedule_ready(
        ready: &[usize],
        free: &Resources,
        requests: &std::collections::BTreeMap<usize, ResNode>,
    ) -> (Vec<usize>, Resources) {
        let mut remaining = free.clone();
        let mut admitted = Vec::new();
        for &idx in ready {
            let req = requests
                .get(&idx)
                .cloned()
                .unwrap_or_else(|| Self::default_resource_node(idx));
            match admit(&req, &remaining) {
                Admit::Run => {
                    remaining = reserve(&remaining, &req);
                    admitted.push(idx);
                }
                Admit::Queue => {}
            }
        }
        (admitted, remaining)
    }

    /// Issue an expiring ownership fence for a ready DAG node (dispatch).
    pub fn lease_dispatch(&mut self, idx: usize, owner: &str, now: u64, ttl: u64) -> TaskLease {
        self.leases
            .dispatch(&Self::persist_id(idx), owner, now, ttl)
    }

    /// Authoritative completion under the live fence. Stale/expired tokens
    /// leave the node unfinished.
    pub fn lease_complete(
        &mut self,
        idx: usize,
        owner: &str,
        fence: u64,
        now: u64,
    ) -> CompleteVerdict {
        let verdict = self
            .leases
            .complete(&Self::persist_id(idx), owner, fence, now);
        if verdict == CompleteVerdict::Accepted {
            if let Some(node) = self.nodes.get_mut(idx) {
                node.completed = true;
            }
        }
        verdict
    }

    /// Apply a node completion into the persisted mission and save it.
    pub fn persist_node_complete(
        persist: &mut PersistedMission,
        persist_dir: &Path,
        idx: usize,
        output: &str,
    ) -> EaiResult<()> {
        let id = Self::persist_id(idx);
        let Some(node) = persist.nodes.get_mut(&id) else {
            return Err(EaiError::governance(format!(
                "persist missing node {id} on complete"
            )));
        };
        // Force authoritative completion regardless of prior Pending/Running —
        // resume must see Completed or it will re-dispatch.
        node.output = Some(output.to_string());
        node.state = NodeTerminal::Completed;
        persist.save(persist_dir)?;
        Ok(())
    }

    /// Seed persist from this DAG when the mission file has no nodes yet.
    pub fn seed_persist(&self, persist: &mut PersistedMission) {
        if !persist.nodes.is_empty() {
            return;
        }
        *persist = self.to_persisted(&persist.mission_id);
    }

    /// Execute the DAG topologically using work-stealing parallel execution
    pub fn execute_dag(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.execute_dag_inner(workspace, blackboard, event_sender, None)
    }

    /// Execute while persisting each successful node completion (resume-safe).
    pub fn execute_dag_persisted(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        persist: &mut MissionPersistCtx<'_>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.seed_persist(persist.mission);
        persist.mission.save(persist.dir)?;
        self.execute_dag_inner(workspace, blackboard, event_sender, Some(persist))
    }

    fn execute_dag_inner(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        mut persist_slot: Option<&mut MissionPersistCtx<'_>>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        use rayon::prelude::*;
        let mut all_evidence = Vec::new();

        let mut executed_count = self.nodes.iter().filter(|node| node.completed).count();
        let total_nodes = self.nodes.len();

        while executed_count < total_nodes {
            if self.is_cancelled(now_unix()) {
                let reports = self.cancel_propagate();
                let detail = if reports.is_empty() {
                    "cancelled".to_string()
                } else {
                    reports.join("; ")
                };
                return Err(EaiError::governance(format!(
                    "DAG_EXECUTION_FAILED: cancel propagated: {detail}"
                )));
            }
            let ready_indices: Vec<usize> = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    !node.completed
                        && node
                            .dependencies
                            .iter()
                            .all(|&dep| self.nodes.get(dep).is_some_and(|node| node.completed))
                })
                .map(|(idx, _)| idx)
                .collect();

            if ready_indices.is_empty() {
                return Err(EaiError::governance(
                    "DAG_EXECUTION_FAILED: unresolved dependencies (cycle or missing task)",
                ));
            }

            // Admit by live resources so oversubscribed fixtures queue (VC-201-024).
            let free = Resources {
                cpu: 8.0,
                gpu_mem_gb: 16.0,
                model_ready: true,
                tool_grants: vec!["exec_command".into()],
            };
            let (ready_indices, _remaining) =
                Self::schedule_ready(&ready_indices, &free, &std::collections::BTreeMap::new());
            if ready_indices.is_empty() {
                return Err(EaiError::governance(
                    "DAG_EXECUTION_FAILED: no ready nodes admitted under resource constraints",
                ));
            }

            // Mark ready nodes Running in persist before the parallel batch.
            if let Some(ctx) = persist_slot.as_mut() {
                for &idx in &ready_indices {
                    let id = Self::persist_id(idx);
                    let _ = ctx.mission.dispatch(&id);
                }
                let _ = ctx.mission.save(ctx.dir);
            }

            // Assign expiring ownership fences before workers run (VC-201-022).
            let now = now_unix();
            let lease_ttl = 3_600;
            let deadline = self
                .cancel
                .token
                .as_ref()
                .map(|t| t.deadline_unix)
                .unwrap_or(now + lease_ttl);
            // T-DEVIN-8: one task-registry scope per mission execution —
            // `cancel_scope` kills every in-flight worker command at once,
            // and the shared signal aborts workers between steps.
            let mission_scope = format!(
                "dag-exec-{}-{}",
                std::process::id(),
                DAG_EXEC_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let mut batch_leases: Vec<(usize, String, u64)> =
                Vec::with_capacity(ready_indices.len());
            let mut batch_signals: Vec<Arc<std::sync::atomic::AtomicBool>> =
                Vec::with_capacity(ready_indices.len());
            for &idx in &ready_indices {
                let owner = format!("worker-{idx}");
                let lease = self.lease_dispatch(idx, &owner, now, lease_ttl);
                let signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.register_cancel_worker_scoped(
                    idx,
                    deadline,
                    Descendant {
                        id: String::new(),
                        kind: WorkerKind::Local,
                        cancellable: true,
                        cancel_scope: Some(mission_scope.clone()),
                        signal: Some(Arc::clone(&signal)),
                    },
                );
                batch_leases.push((idx, owner, lease.fence));
                batch_signals.push(signal);
            }

            let ws = workspace.to_path_buf();
            let bb = Arc::clone(blackboard);
            let tx = event_sender.clone();

            // Writer isolation (T-DEVIN-6): every mutating worker runs in
            // its own synced scope — never concurrent shell commands in
            // one directory. Writes fold back sequentially below.
            let isolation = crate::writer_isolation::WriterIsolation::new(&ws)?;
            let mut batch_scopes = Vec::with_capacity(batch_leases.len());
            for (_, owner, _) in &batch_leases {
                batch_scopes.push(isolation.scope(owner)?);
            }

            let batch_results: Vec<NodeRun> = batch_leases
                .into_par_iter()
                .zip(batch_scopes)
                .zip(batch_signals)
                .map(|(((idx, owner, fence), scope), signal)| {
                    let node = &self.nodes[idx];
                    let node_ws = scope.dir.clone();
                    let start = std::time::Instant::now();
                    // Mid-batch cancel check (T-DEVIN-8): a mission cancel can
                    // only arrive via the task scope (propagate / external
                    // stop), the shared signal, or the token deadline —
                    // the dag itself is mutably borrowed for the whole run.
                    let manager =
                        crate::susi_core::task_manager::SwarmTaskManager::global();
                    let scope_for_check = mission_scope.clone();
                    let cancelled = move || {
                        signal.load(std::sync::atomic::Ordering::Acquire)
                            || manager.is_scope_cancelled(&scope_for_check)
                            || now_unix() >= deadline
                    };
                    if cancelled() {
                        return NodeRun {
                            idx,
                            res: Err(EaiError::governance("worker cancelled")),
                            elapsed_ms: start.elapsed().as_millis() as u64,
                            calls: Vec::new(),
                            owner,
                            fence,
                            scope,
                            cancelled: true,
                        };
                    }
                    let _ = tx.send(crate::susi_core::bus::SwarmEventType::AgentStarted {
                        agent_name: node.title.clone(),
                    });

                    // Mandate 48: in SUSI's own tree the node that turns model
                    // output into shell commands carries the self-build contract.
                    let prompt = crate::susi_core::self_build::brief_task(&node_ws, &format!(
                        "Execute task node '{}': {}. If you need to execute a shell command, provide it in a ```bash codeblock. The command must perform every requested side effect: printing intended file content is not file creation. For file writes, write the named workspace path and then verify that exact path and its contents.",
                        node.title, node.goal
                    ));
                    // Deep path, same rule as plan_steps: the node template is
                    // full of capability words and clears Tier-0's support
                    // gate, so a trained reflex could answer `ACTION:
                    // write_file` with no ```bash block — the node would
                    // "complete" having executed nothing (Claude's measured
                    // 0.60–0.64 support on everyday intents).
                    let mut res =
                        crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
                            &prompt, &node_ws,
                        );

                    let mut executed_scripts = String::new();
                    // Receipt arguments this node produced (JSON of the
                    // exec_command argument), for binding its own evidence.
                    let mut node_calls = Vec::new();
                    for block in res.split("```").skip(1).step_by(2) {
                        if cancelled() {
                            break;
                        }
                        if let Some(cmd) = shell_block_command(block) {
                            eprintln!("[DAG Agent] Detected shell block. Executing native tool...");
                            let wrapped_cmd = format!("sh -c '{}'", cmd.replace('\'', "'\\''"));
                            // cancel_scope binds the spawned process to this
                            // mission's cancellation scope: propagate/kill
                            // terminates the live child, not just the record.
                            let call = serde_json::json!({
                                "command": wrapped_cmd,
                                "cancel_scope": mission_scope,
                            });
                            node_calls.push(call.to_string());
                            let result = crate::susi_core::plane_bus::tools::execute_tool("exec_command", &call, &node_ws).unwrap_or_else(|e| format!("[Error] {e}"));
                            executed_scripts.push_str(&format!("\n\nExecution Result for `{cmd}`:\n{}\n", result));
                        }
                    }

                    if !executed_scripts.is_empty() {
                        res.push_str(&executed_scripts);
                        if let Some(citations) = crate::susi_core::capture::EvidenceSession::auto_format_truth(&node_ws) {
                            res.push_str("\n\n");
                            res.push_str(&citations);
                        }
                    }

                    let elapsed = start.elapsed().as_millis() as u64;
                    let was_cancelled = cancelled();
                    NodeRun {
                        idx,
                        res: Ok(res),
                        elapsed_ms: elapsed,
                        calls: node_calls,
                        owner,
                        fence,
                        scope,
                        cancelled: was_cancelled,
                    }
                })
                .collect();

            // Fold each scope's writes back into the shared workspace,
            // sequentially and in node order — deterministic, reviewable
            // conflicts instead of silently lost writes.
            let mut write_conflicts: Vec<String> = Vec::new();
            let mut conflicted_nodes: BTreeSet<usize> = BTreeSet::new();
            for run in &batch_results {
                // Cancelled workers' private writes are discarded, never
                // folded — a killed command's partial output must not land.
                if run.cancelled {
                    isolation.cleanup(&run.scope);
                    continue;
                }
                match isolation.fold(&run.scope) {
                    Ok(crate::writer_isolation::FoldOutcome::Clean { .. }) => {}
                    Ok(crate::writer_isolation::FoldOutcome::Conflict { paths }) => {
                        write_conflicts.push(format!(
                            "n{}:{}",
                            run.idx,
                            paths.into_iter().collect::<Vec<_>>().join(",")
                        ));
                        conflicted_nodes.insert(run.idx);
                    }
                    Err(e) => {
                        for rest in &batch_results {
                            if !rest.cancelled {
                                isolation.cleanup(&rest.scope);
                            }
                        }
                        return Err(e);
                    }
                }
            }
            if !write_conflicts.is_empty() {
                let detail = write_conflicts.join(";");
                if let Some(ctx) = persist_slot.as_mut() {
                    for &idx in &conflicted_nodes {
                        let id = Self::persist_id(idx);
                        if let Some(node) = ctx.mission.nodes.get_mut(&id) {
                            node.state = NodeTerminal::Failed;
                            node.output = Some(format!("[WRITE_CONFLICT] {detail}"));
                        }
                    }
                    let _ = ctx.mission.save(ctx.dir);
                }
                for run in &batch_results {
                    if !run.cancelled {
                        isolation.cleanup(&run.scope);
                    }
                }
                return Err(EaiError::governance(format!(
                    "DAG_EXECUTION_FAILED: write conflict needs review: {detail}"
                )));
            }

            // A cancel that arrived mid-batch: workers aborted between steps
            // and their scoped commands were killed. Propagate to terminate
            // the rest, then refuse to complete the batch.
            if batch_results.iter().any(|run| run.cancelled) || self.is_cancelled(now_unix()) {
                let reports = self.cancel_propagate();
                let mut cancelled_nodes: Vec<usize> = batch_results
                    .iter()
                    .filter(|run| run.cancelled)
                    .map(|run| run.idx)
                    .collect();
                cancelled_nodes.sort_unstable();
                if let Some(ctx) = persist_slot.as_mut() {
                    for &idx in &cancelled_nodes {
                        let id = Self::persist_id(idx);
                        if let Some(node) = ctx.mission.nodes.get_mut(&id) {
                            node.state = NodeTerminal::Failed;
                            node.output = Some("[CANCELLED] mission cancel propagated".to_string());
                        }
                    }
                    let _ = ctx.mission.save(ctx.dir);
                }
                for run in &batch_results {
                    if !run.cancelled {
                        isolation.cleanup(&run.scope);
                    }
                }
                let detail = if reports.is_empty() {
                    "cancelled".to_string()
                } else {
                    reports.join("; ")
                };
                return Err(EaiError::governance(format!(
                    "DAG_EXECUTION_FAILED: cancel propagated mid-batch (nodes {cancelled_nodes:?}): {detail}"
                )));
            }
            for run in &batch_results {
                if !run.cancelled {
                    isolation.cleanup(&run.scope);
                }
            }

            for run in batch_results {
                let NodeRun {
                    idx,
                    res,
                    elapsed_ms: elapsed,
                    calls: node_calls,
                    owner,
                    fence,
                    ..
                } = run;
                if let Ok(output) = res {
                    // Crown path: citation answers resolve from the live ledger;
                    // narratives without required citations fail TRUTH_UNVERIFIED.
                    match crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &self.nodes[idx].goal,
                        &self.nodes[idx].title,
                        &output,
                        workspace,
                    ) {
                        Ok(verified) => {
                            // Classify exec_command side effects before fencing
                            // completion so crash resume can reconcile (VC-201-023).
                            if !node_calls.is_empty() {
                                let prior = self.side_effects.get(&Self::persist_id(idx)).cloned();
                                let _ = self.record_side_effect(
                                    idx,
                                    "exec_command",
                                    prior.as_ref(),
                                );
                            }
                            let complete_now = now_unix();
                            match self.lease_complete(idx, &owner, fence, complete_now) {
                                CompleteVerdict::Accepted => {}
                                CompleteVerdict::StaleFence
                                | CompleteVerdict::NotOwner
                                | CompleteVerdict::Expired
                                | CompleteVerdict::UnknownTask => {
                                    return Err(EaiError::governance(format!(
                                        "DAG_EXECUTION_FAILED: lease fence refused completion of n{idx}"
                                    )));
                                }
                            }
                            let _ =
                                event_sender.send(crate::susi_core::bus::SwarmEventType::AgentCompleted {
                                    agent_name: self.nodes[idx].title.clone(),
                                    elapsed_ms: elapsed,
                                });
                            // lease_complete already marked completed=true
                            executed_count += 1;
                            bb.insert(format!("TaskNode_{}", idx), verified.clone());

                            if let Some(ctx) = persist_slot.as_mut() {
                                let _ = Self::persist_node_complete(
                                    ctx.mission,
                                    ctx.dir,
                                    idx,
                                    &verified,
                                );
                            }

                            // Pillar Evidence: only store Claim trails that assess Verified
                            // (ToolReceipt-bound). Naked AgentObservation IR is not a trail.
                            if let Some(session) =
                                crate::susi_core::capture::EvidenceSession::for_workspace(workspace)
                            {
                                // This node's own successful call — not the
                                // session's first receipt, which bound every
                                // node to whatever tool ran earliest (often
                                // another node's, or an unrelated call).
                                if let Some(receipt) = session.receipts().into_iter().rev().find(|r| {
                                    r.successful
                                        && r.tool == "exec_command"
                                        && node_calls.contains(&r.arguments)
                                }) {
                                    if let Ok(record) = session.bind_receipt(
                                        &receipt.id,
                                        &self.nodes[idx].title,
                                        crate::susi_core::evidence::Claim {
                                            subject: self.nodes[idx].title.clone(),
                                            predicate: "observed".to_string(),
                                            value: receipt.output_hash.clone(),
                                        },
                                    ) {
                                        if record.verify_reality(workspace) {
                                            all_evidence.push(record);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let msg = format!("[TRUTH_VIOLATION] {}", e);
                            bb.insert(format!("TaskNode_{}", idx), msg);
                            return Err(e);
                        }
                    }
                }
            }
        }

        Ok(all_evidence)
    }
}

/// The command in a fenced block tagged `bash`, `sh`, or `shell`. The tag
/// is the whole first line: chained `trim_start_matches` calls used to eat
/// the `sh` of `shell` and run a command starting with `ell`.
fn shell_block_command(block: &str) -> Option<&str> {
    let (tag, body) = block.trim_start().split_once('\n')?;
    if !matches!(tag.trim(), "bash" | "sh" | "shell") {
        return None;
    }
    let cmd = body.trim();
    (!cmd.is_empty() && !cmd.starts_with('!')).then_some(cmd)
}

/// Hook body registered into `susi_gawd_agents::dag_hooks` by [`crate::init`].
///
/// Loads any prior [`PersistedMission`] under `<workspace>/.susi/missions`,
/// resumes completed nodes without re-running them, and persists each new
/// completion so a crash mid-mission does not treat unfinished work as done.
pub fn dispatch_mission_dag(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let persist_dir = PersistedMission::missions_dir(workspace);
    let mission_id = PersistedMission::mission_id_for_goal(goal);
    let mut persist = PersistedMission::load_or_new(&persist_dir, &mission_id)
        .unwrap_or_else(|_| PersistedMission::new(mission_id.clone()));

    let mut dag = if persist.nodes.is_empty() {
        let dag = MissionDag::new(goal);
        dag.seed_persist(&mut persist);
        let _ = persist.save(&persist_dir);
        dag
    } else {
        MissionDag::from_persisted(&persist)
    };

    let (event_tx, _event_rx) = crate::susi_core::bus::create_swarm_bus();
    let mut persist_ctx = MissionPersistCtx {
        mission: &mut persist,
        dir: &persist_dir,
    };
    match dag.execute_dag_persisted(workspace, blackboard, &event_tx, &mut persist_ctx) {
        Ok(evidence_records) => {
            for record in &evidence_records {
                let payload =
                    serde_json::to_string(record).unwrap_or_else(|_| record.render_for_gemi());
                blackboard.insert(format!("EvidenceRecord::{}", record.claim.subject), payload);
            }
            if let Some(record) = evidence_records.first() {
                results.push(("MissionDag".to_string(), record.claim.value.clone()));
            }
        }
        Err(e) => {
            results.push((
                "MissionDag".to_string(),
                format!("[DAG_EXECUTION_FAILED] {}", e),
            ));
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_gawd_agents::HighDensityContextStore;

    #[test]
    fn invalid_dependencies_fail_without_completing_tasks() {
        for dependency in [0, 99] {
            let mut dag = MissionDag::new("inspect");
            dag.nodes[0].dependencies = vec![dependency];
            let board = Arc::new(HighDensityContextStore::new(1024));
            let (tx, rx) = crate::susi_core::bus::create_swarm_bus();
            assert!(dag.execute_dag(Path::new("."), &board, &tx).is_err());
            assert!(!dag.nodes[0].completed);
            assert!(board.is_empty());
            assert!(rx.is_empty());
        }
    }

    #[test]
    fn completed_graph_is_not_executed_again() {
        let mut dag = MissionDag::new("inspect");
        dag.nodes[0].completed = true;
        let board = Arc::new(HighDensityContextStore::new(1024));
        let (tx, rx) = crate::susi_core::bus::create_swarm_bus();
        assert!(dag
            .execute_dag(Path::new("."), &board, &tx)
            .unwrap()
            .is_empty());
        assert!(rx.is_empty());
    }
}

#[cfg(test)]
mod shell_block_tests {
    use super::shell_block_command;

    #[test]
    fn every_tag_yields_the_whole_command() {
        assert_eq!(shell_block_command("bash\necho hi\n"), Some("echo hi"));
        assert_eq!(shell_block_command("sh\nls -la"), Some("ls -la"));
        assert_eq!(shell_block_command("shell\nls -la"), Some("ls -la"));
        assert_eq!(shell_block_command("shellscript\nls"), None);
        assert_eq!(shell_block_command("rust\nfn main() {}"), None);
        assert_eq!(shell_block_command("bash\n!sudo reboot"), None);
        assert_eq!(shell_block_command("bash\n   "), None);
    }
}
