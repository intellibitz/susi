// Dependency-ordered task graph: agents can spawn sub-tasks with
// dependencies on parent tasks, executed in ready-batches via rayon.

use crate::cancel_propagate::{
    run_cancellable, CancelBus, CancelToken, CancellableResult, Descendant, WorkerKind,
};
use crate::capacity_admission::LiveAdmission;
use crate::fair_queue::{EnqueueResult, FairQueue, QueueLimits, QueuedMission};
use crate::independent_verify::{verification_satisfied, ReviewConclusion};
use crate::joint_consensus::{overlapping_disjoint_blocked, Electorate, MembershipTransition};
use crate::mission_persist::{NodeTerminal, PersistedMission, PersistedNode};
use crate::mission_resume::{DagNodeView, MissionView, NodeView};
use crate::node_enrollment::{enroll, Enrollment};
use crate::resource_schedule::{admit, reserve, Admit, DagNode as ResNode, Resources};
use crate::role_select::{select_roles, AgentEvidence, RoleAssignment, SelectError};
use crate::side_effect_journal::DispatchDecision;
use crate::side_effects::{reconcile_dispatch_side_effect, ActionOutcome};
use crate::susi_core::evidence::EvidenceRecord;
use crate::susi_error::{EaiError, EaiResult};
use crate::task_lease::{AuthorityToken, CompleteVerdict, LeaseTable, OwnershipEpoch, TaskLease};
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
    /// Mission/coordinator authority generation.  It advances on every
    /// persisted recovery before a worker can be redispatched.
    pub ownership: OwnershipEpoch,
    /// Side-effect outcomes keyed by persist id (VC-201-023).
    pub side_effects: std::collections::BTreeMap<String, ActionOutcome>,
    /// Cancellation bus for workers/peers (VC-201-026).
    pub cancel: CancelBus,
    /// Side-effect intent journal (T-DEVIN-10): every mutating call is
    /// recorded before dispatch; pending entries become Uncertain on crash
    /// and block replay until reconciled.
    pub intent_journal: std::sync::Mutex<crate::side_effect_journal::IntentJournal>,
    /// Fair multi-mission admit queue (VC-201-025).
    pub fair_queue: FairQueue,
    /// Active swarm roster; changes commit only under joint consensus (VC-201-032).
    pub roster: Electorate,
    /// In-flight membership transition awaiting joint quorum.
    pub pending_membership: Option<MembershipTransition>,
    /// Admission capacity override (T-DEVIN-12). `None` measures the host
    /// each scheduling round; tests pin a fixed snapshot for determinism.
    pub capacity: Option<Resources>,
    /// Optional shared admission ledger. Production defaults to the
    /// process-wide host ledger; tests and embedding callers may inject a
    /// small measured inventory.
    pub live_admission: Option<Arc<LiveAdmission>>,
    /// Per-node measured resource requirements. Missing entries use the
    /// conservative default request rather than a fixed host assumption.
    pub resource_requests: std::collections::BTreeMap<usize, ResNode>,
    /// Independent review receipts accepted for verify nodes. Keeping the
    /// conclusion on the DAG prevents a transient model response from being
    /// mistaken for durable verification evidence.
    pub verification_receipts:
        std::collections::BTreeMap<usize, crate::independent_verify::ReviewConclusion>,
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
    /// Fence was lost before/during the run (T-DEVIN-9): its private writes
    /// are discarded, never folded, and the node is refused completion.
    stale_fence: bool,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Model adapter used by the DAG's cancellable worker boundary.
pub(crate) type DagModelGenerator = Arc<dyn Fn(&str, &Path) -> String + Send + Sync + 'static>;

/// Tool adapter used by the DAG's cancellable mutation boundary.
pub(crate) type DagToolExecutor =
    Arc<dyn Fn(&str, &serde_json::Value, &Path) -> EaiResult<String> + Send + Sync + 'static>;

fn production_model_generator() -> DagModelGenerator {
    Arc::new(|prompt, workspace| {
        crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(prompt, workspace)
    })
}

fn production_tool_executor() -> DagToolExecutor {
    Arc::new(|name, args, workspace| {
        crate::susi_core::plane_bus::tools::execute_tool(name, args, workspace)
    })
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
            ownership: OwnershipEpoch::default(),
            side_effects: std::collections::BTreeMap::new(),
            cancel: CancelBus::default(),
            intent_journal: std::sync::Mutex::new(crate::side_effect_journal::IntentJournal::new()),
            fair_queue: FairQueue::new(QueueLimits::default()),
            roster: Electorate(BTreeSet::new()),
            pending_membership: None,
            capacity: None,
            live_admission: None,
            resource_requests: std::collections::BTreeMap::new(),
            verification_receipts: std::collections::BTreeMap::new(),
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
    /// Carries the lease table so fencing survives restart (T-DEVIN-9).
    #[must_use]
    pub fn to_persisted(&self, mission_id: &str) -> PersistedMission {
        let mut mission = PersistedMission::new(mission_id);
        mission.leases = self.leases.clone();
        mission.ownership = self.ownership;
        mission.recovery = self.leases.recovery.clone();
        mission.side_effect_journal = self
            .intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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
        let mut dag = Self {
            nodes,
            leases: LeaseTable::new(),
            ownership: mission.ownership,
            side_effects: std::collections::BTreeMap::new(),
            cancel: CancelBus::default(),
            intent_journal: std::sync::Mutex::new(crate::side_effect_journal::IntentJournal::new()),
            fair_queue: FairQueue::new(QueueLimits::default()),
            roster: Electorate(BTreeSet::new()),
            pending_membership: None,
            capacity: None,
            live_admission: None,
            resource_requests: std::collections::BTreeMap::new(),
            verification_receipts: std::collections::BTreeMap::new(),
        };
        // Durable state survives the crash (T-DEVIN-9/10): fences stay
        // monotonic so stale tokens can't collide, and intents recorded
        // pre-dispatch but never observed become Uncertain — replay is
        // refused until each is reconciled.
        dag.leases.adopt(&mission.leases);
        dag.ownership = mission.ownership;
        {
            let mut journal = dag.intent_journal.lock().unwrap_or_else(|e| e.into_inner());
            journal.adopt(&mission.side_effect_journal);
            journal.reconcile_after_crash();
        }
        dag
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
    /// A node with unreconciled journal intents is never replayed —
    /// reconcile the uncertain dispatch first (T-DEVIN-10).
    #[must_use]
    pub fn may_retry_node(&self, idx: usize) -> bool {
        let id = Self::persist_id(idx);
        let journal_ok = self
            .intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .may_replay_node(&id);
        journal_ok
            && self
                .side_effects
                .get(&id)
                .is_none_or(|o| crate::side_effects::may_retry_after_crash(o.class))
    }

    /// Record dispatch intent for a node's tool call BEFORE execution;
    /// returns the intent id to mark executed on success.
    pub fn record_dispatch_intent(&self, idx: usize, tool: &str, args: &str) -> String {
        self.intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_intent(&Self::persist_id(idx), tool, args, now_unix())
    }

    /// Prepare one tool operation at the production dispatch boundary.  A
    /// mutating operation must provide a stable key that survives restart;
    /// acknowledged operations return a durable receipt and are not invoked
    /// again, while uncertain operations remain blocked for reconciliation.
    pub fn prepare_dispatch_intent(
        &self,
        idx: usize,
        tool: &str,
        args: &str,
        idempotency_key: Option<&str>,
    ) -> DispatchDecision {
        self.intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prepare_dispatch(
                &Self::persist_id(idx),
                tool,
                args,
                idempotency_key,
                now_unix(),
            )
    }

    /// Mark an intent's dispatch observed-complete.
    pub fn mark_intent_executed(&self, intent_id: &str) {
        self.intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_executed(intent_id);
    }

    /// Publish the receipt that follows a successful external operation.
    pub fn mark_intent_executed_with_receipt(&self, intent_id: &str, output: &str) {
        self.intent_journal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_executed_with_receipt(intent_id, output, now_unix());
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
            Some(t) => {
                t.cancelled
                    || self.cancel.handle().is_cancelled()
                    || t.remaining_secs(now).is_none()
            }
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
        let Some(implementer) = self.nodes.first().and_then(|n| n.assigned_agent.as_deref()) else {
            return false;
        };
        if !verification_satisfied(conclusion, implementer) {
            return false;
        }
        let verify_idx = self
            .nodes
            .iter()
            .position(|n| n.title == "Independent Verify");
        let Some(idx) = verify_idx else {
            return false;
        };
        let Some(node) = self.nodes.get_mut(idx) else {
            return false;
        };
        // Assigned verifier must match the conclusion reviewer. An absent
        // assignment is not an authenticated identity.
        if node.assigned_agent.as_deref() != Some(conclusion.reviewer.as_str()) {
            return false;
        }
        node.completed = true;
        self.verification_receipts.insert(idx, conclusion.clone());
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
            mem_gb: 0.0,
            subprocesses: 1,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        }
    }

    /// Inject one shared admission ledger, preserving atomic reservations
    /// when several mission DAGs execute concurrently.
    #[must_use]
    pub fn with_live_admission(mut self, admission: Arc<LiveAdmission>) -> Self {
        self.live_admission = Some(admission);
        self
    }

    /// Replace the live admission ledger on an existing mission.
    pub fn set_live_admission(&mut self, admission: Arc<LiveAdmission>) {
        self.live_admission = Some(admission);
    }

    /// Record measured resource demand for a node. The request is consumed by
    /// the production execution path, not only by a unit-test helper.
    pub fn set_resource_request(&mut self, idx: usize, request: ResNode) {
        self.resource_requests.insert(idx, request);
    }

    /// Admit the supplied ready nodes through the same production ledger used
    /// by [`Self::execute_dag`]. Deferred nodes stay queued in that ledger;
    /// callers must not mark them complete until a later pass admits them.
    pub fn admit_ready_nodes(
        &mut self,
        mission: &str,
        ready: &[usize],
    ) -> crate::capacity_admission::AdmissionBatch {
        let requests: Vec<(usize, ResNode)> = ready
            .iter()
            .map(|&idx| {
                (
                    idx,
                    self.resource_requests
                        .get(&idx)
                        .cloned()
                        .unwrap_or_else(|| Self::default_resource_node(idx)),
                )
            })
            .collect();
        self.live_admission().admit_batch(mission, &requests)
    }

    fn live_admission(&mut self) -> Arc<LiveAdmission> {
        if let Some(admission) = self.live_admission.as_ref() {
            return Arc::clone(admission);
        }
        let admission = self
            .capacity
            .clone()
            .map(crate::capacity_admission::LiveAdmission::fixed)
            .unwrap_or_else(crate::capacity_admission::host_admission);
        let admission = Arc::new(admission);
        self.live_admission = Some(Arc::clone(&admission));
        admission
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
            .dispatch_in_epoch(&Self::persist_id(idx), owner, now, ttl, self.ownership)
    }

    /// Authority token for a currently leased node.
    #[must_use]
    pub fn lease_authority(&self, idx: usize) -> Option<AuthorityToken> {
        self.leases.authority_for(&Self::persist_id(idx))
    }

    /// Check a worker's authority before a mutating boundary.
    #[must_use]
    pub fn check_mutation_authority(
        &self,
        idx: usize,
        owner: &str,
        token: AuthorityToken,
        now: u64,
    ) -> CompleteVerdict {
        self.leases
            .check_authority(&Self::persist_id(idx), owner, token, now)
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
        let output = output.to_string();
        persist.transaction(persist_dir, |mission| {
            let Some(node) = mission.nodes.get_mut(&id) else {
                return Err(EaiError::governance(format!(
                    "persist missing node {id} on complete"
                )));
            };
            // Force authoritative completion regardless of prior Pending/Running —
            // resume must see Completed or it will re-dispatch.
            node.output = Some(output);
            node.state = NodeTerminal::Completed;
            mission.recovery = mission.leases.recovery.clone();
            Ok(())
        })
    }

    /// Seed persist from this DAG when the mission file has no nodes yet.
    pub fn seed_persist(&self, persist: &mut PersistedMission) {
        if !persist.nodes.is_empty() {
            return;
        }
        // Seed only the graph.  Replacing the whole record here would erase
        // leases, epochs, recovery scopes or an intent journal loaded from a
        // crash-recovery file before the first node is added.
        persist.nodes = self.to_persisted(&persist.mission_id).nodes;
    }

    /// Execute the DAG topologically using work-stealing parallel execution
    pub fn execute_dag(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.execute_dag_inner(
            workspace,
            blackboard,
            event_sender,
            None,
            production_model_generator(),
            production_tool_executor(),
        )
    }

    /// Execute while persisting each successful node completion (resume-safe).
    pub fn execute_dag_persisted(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        persist: &mut MissionPersistCtx<'_>,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.execute_dag_persisted_with_backends(
            workspace,
            blackboard,
            event_sender,
            persist,
            production_model_generator(),
            production_tool_executor(),
        )
    }

    #[allow(clippy::too_many_arguments)] // persistence context and two injected production adapters are the complete dispatch boundary
    pub(crate) fn execute_dag_persisted_with_backends(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        persist: &mut MissionPersistCtx<'_>,
        model_generator: DagModelGenerator,
        tool_executor: DagToolExecutor,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        self.seed_persist(persist.mission);
        // A recorded terminal `Failed` verdict is the mission's outcome:
        // report it cleanly, naming the node and its recorded output,
        // rather than re-running to a dispatch refusal wedge (T-DEEPSEEK-93).
        if let crate::mission_persist::ResumeVerdict::TerminalFailed(failed) =
            persist.mission.resume_verdict()
        {
            let detail = failed
                .iter()
                .map(|(id, out)| format!("{id}: {out}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(EaiError::governance(format!(
                "DAG_EXECUTION_FAILED: mission {} resumed with recorded failure at {detail}",
                persist.mission.mission_id
            )));
        }
        // Recovery is an atomic authority transition.  It advances both
        // epochs, records the restart, invalidates old leases, folds
        // crash-interrupted `Running` nodes back to dispatchable `Pending`
        // (recording the fold on the mission), and turns any in-flight
        // scope into a cancellation/quarantine candidate before a new
        // worker can be dispatched.
        let stale_scopes: Vec<String> = persist
            .mission
            .leases
            .recovery
            .active_scopes
            .iter()
            .cloned()
            .collect();
        let coordinator = format!("coordinator-{}", std::process::id());
        let now = now_unix();
        let next_ownership = persist.mission.ownership.next();
        persist.mission.transaction(persist.dir, |mission| {
            mission.ownership = next_ownership;
            mission.coordinator = coordinator.clone();
            mission.leases.begin_recovery(next_ownership, now);
            mission.leases.recovery.active_scopes.clear();
            mission.recovery = mission.leases.recovery.clone();
            mission.reconcile_interrupted(now);
            mission.side_effect_journal.reconcile_after_crash();
            Ok(())
        })?;
        for scope in stale_scopes {
            crate::susi_core::task_manager::SwarmTaskManager::global().cancel_scope(&scope);
        }
        self.ownership = next_ownership;
        self.leases = persist.mission.leases.clone();
        let isolation = crate::writer_isolation::WriterIsolation::new(workspace)?;
        isolation.retire_stale_scopes(&format!(
            "{}-{}",
            next_ownership.mission, next_ownership.coordinator
        ))?;
        self.execute_dag_inner(
            workspace,
            blackboard,
            event_sender,
            Some(persist),
            model_generator,
            tool_executor,
        )
    }

    #[allow(clippy::too_many_arguments)] // the inner loop carries its workspace, event, persistence and adapter boundaries explicitly
    fn execute_dag_inner(
        &mut self,
        workspace: &Path,
        blackboard: &MissionBlackboard,
        event_sender: &flume::Sender<crate::susi_core::bus::SwarmEventType>,
        mut persist_slot: Option<&mut MissionPersistCtx<'_>>,
        model_generator: DagModelGenerator,
        tool_executor: DagToolExecutor,
    ) -> EaiResult<Vec<EvidenceRecord>> {
        use rayon::prelude::*;
        let mut all_evidence = Vec::new();

        let mut executed_count = self.nodes.iter().filter(|node| node.completed).count();
        let total_nodes = self.nodes.len();
        let admission_mission = format!("dag-{:p}", self);

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

            // Admit against a fresh shared inventory snapshot. Deferred nodes
            // remain in the live queue and are retried after this batch's
            // reservations release; they are never marked complete here.
            let admission_batch = self.admit_ready_nodes(&admission_mission, &ready_indices);
            let ready_indices = admission_batch.admitted_indices();
            let deferred = admission_batch.deferred.clone();
            let batch_reservations: Vec<_> = admission_batch
                .admitted
                .into_iter()
                .map(|(_, reservation)| reservation)
                .collect();
            if !deferred.is_empty() {
                eprintln!(
                    "[DAG Admission] deferred {} required node(s) for a later drain: {}",
                    deferred.len(),
                    deferred
                        .iter()
                        .map(|idx| Self::persist_id(*idx))
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
            if ready_indices.is_empty() {
                if let Some(blocked) = admission_batch.blocked {
                    return Err(EaiError::governance(format!(
                        "DAG_EXECUTION_FAILED: no ready nodes admitted under resource constraints; node {} remains queued: {}",
                        blocked.node_id, blocked.reason
                    )));
                }
                return Err(EaiError::governance(
                    "DAG_EXECUTION_FAILED: no ready nodes admitted under resource constraints",
                ));
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
                "dag-exec-{}-{}-{}-{}",
                std::process::id(),
                self.ownership.mission,
                self.ownership.coordinator,
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
            let cancellation = self.cancel.handle();
            cancellation.register_scope(&mission_scope);

            // Publish node-running state, leases, recovery scope and intent
            // journal together before any worker scope or tool can mutate.
            if let Some(ctx) = persist_slot.as_mut() {
                let leases = self.leases.clone();
                let journal = self
                    .intent_journal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let ownership = self.ownership;
                ctx.mission.transaction(ctx.dir, |mission| {
                    for &idx in &ready_indices {
                        mission.dispatch(&Self::persist_id(idx))?;
                    }
                    mission.ownership = ownership;
                    mission.leases = leases;
                    mission.recovery = mission.leases.recovery.clone();
                    mission
                        .leases
                        .recovery
                        .active_scopes
                        .insert(mission_scope.clone());
                    mission.recovery = mission.leases.recovery.clone();
                    mission.side_effect_journal = journal;
                    Ok(())
                })?;
            }

            let ws = workspace.to_path_buf();
            let bb = Arc::clone(blackboard);
            let tx = event_sender.clone();

            // Writer isolation (T-DEVIN-6): every mutating worker runs in
            // its own synced scope — never concurrent shell commands in
            // one directory. Writes fold back sequentially below.
            let isolation = crate::writer_isolation::WriterIsolation::new(&ws)?;
            let mut batch_scopes = Vec::with_capacity(batch_leases.len());
            for (_, owner, fence) in &batch_leases {
                // Include the fence in the directory identity.  A delayed
                // process from an earlier lease can therefore only write to
                // its quarantined private scope, never the replacement's.
                let scope_owner = format!("{owner}-f{fence}");
                batch_scopes.push(isolation.scope(&scope_owner)?);
            }

            let mut batch_results: Vec<NodeRun> = batch_leases
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
                    let cancellation_for_check = cancellation.clone();
                    let cancelled = move || {
                        signal.load(std::sync::atomic::Ordering::Acquire)
                            || manager.is_scope_cancelled(&scope_for_check)
                            || cancellation_for_check.should_stop()
                            || now_unix() >= deadline
                    };
                    // Fence check BEFORE the first mutating operation
                    // (T-DEVIN-9): rejecting only at completion cannot undo
                    // writes the stale worker already made.
                    let node_id = Self::persist_id(idx);
                    let stale = self.leases.check_fence(
                        &node_id, &owner, fence, now_unix(),
                    ) != CompleteVerdict::Accepted;
                    if cancelled() || stale {
                        return NodeRun {
                            idx,
                            res: Err(EaiError::governance(if stale {
                                "worker lost fence before dispatch"
                            } else {
                                "worker cancelled"
                            })),
                            elapsed_ms: start.elapsed().as_millis() as u64,
                            calls: Vec::new(),
                            owner,
                            fence,
                            scope,
                            cancelled: cancelled(),
                            stale_fence: stale,
                        };
                    }
                    let _ = tx.send(crate::susi_core::bus::SwarmEventType::AgentStarted {
                        agent_name: node.title.clone(),
                    });

                    // Mandate 48: in SUSI's own tree the node that turns model
                    // output into shell commands carries the self-build contract.
                    let title = node.title.clone();
                    let goal = node.goal.clone();
                    let prompt = crate::susi_core::self_build::brief_task(&node_ws, &format!(
                        "Execute task node '{}': {}. If you need to execute a shell command, provide it in a ```bash codeblock. The command must perform every requested side effect: printing intended file content is not file creation. For file writes, write the named workspace path and then verify that exact path and its contents.",
                        title, goal
                    ));
                    // Deep path, same rule as plan_steps: the node template is
                    // full of capability words and clears Tier-0's support
                    // gate, so a trained reflex could answer `ACTION:
                    // write_file` with no ```bash block — the node would
                    // "complete" having executed nothing (Claude's measured
                    // 0.60–0.64 support on everyday intents).
                    let model_workspace = node_ws.clone();
                    let model_cancel = cancellation.clone();
                    let model_prompt = prompt.clone();
                    let model_generator = Arc::clone(&model_generator);
                    let res = match run_cancellable(&model_cancel, move || {
                        model_generator(&model_prompt, &model_workspace)
                    }) {
                        Ok(CancellableResult::Completed(output)) => Ok(output),
                        Ok(CancellableResult::Cancelled) => Err(EaiError::governance(
                            "worker cancelled while waiting for model response",
                        )),
                        Ok(CancellableResult::DeadlineExceeded) => Err(EaiError::governance(
                            "worker deadline expired while waiting for model response",
                        )),
                        Err(error) => Err(error),
                    };

                    let elapsed = start.elapsed().as_millis() as u64;
                    NodeRun {
                        idx,
                        res,
                        elapsed_ms: elapsed,
                        // Tool calls are intentionally executed after the
                        // parallel model phase.  That keeps the durable
                        // intent write, authority check and native tool call
                        // in one sequential mutation boundary.
                        calls: Vec::new(),
                        owner,
                        fence,
                        scope,
                        cancelled: cancelled() || cancellation.should_stop(),
                        stale_fence: stale,
                    }
                })
                .collect();

            // Execute mutating tool calls only after the model phase has
            // joined.  Each call now has a durable intent and an authority
            // check immediately before the native tool boundary; a stale
            // worker therefore cannot write and only its private scope can
            // be discarded.
            for run in &mut batch_results {
                if run.cancelled || run.stale_fence {
                    continue;
                }
                let generated = match &run.res {
                    Ok(output) => output.clone(),
                    Err(_) => continue,
                };
                let node_id = Self::persist_id(run.idx);
                let node_ws = run.scope.dir.clone();
                let mut executed_scripts = String::new();
                let mut node_calls = Vec::new();
                for (block_index, block) in generated.split("```").skip(1).step_by(2).enumerate() {
                    let manager = crate::susi_core::task_manager::SwarmTaskManager::global();
                    if manager.is_scope_cancelled(&mission_scope) || self.is_cancelled(now_unix()) {
                        run.cancelled = true;
                        break;
                    }
                    let token = AuthorityToken {
                        epoch: self.ownership,
                        fence: run.fence,
                    };
                    if self
                        .leases
                        .check_authority(&node_id, &run.owner, token, now_unix())
                        != CompleteVerdict::Accepted
                    {
                        run.stale_fence = true;
                        break;
                    }
                    let Some(cmd) = shell_block_command(block) else {
                        continue;
                    };
                    eprintln!("[DAG Agent] Detected shell block. Executing native tool...");
                    let wrapped_cmd = format!("sh -c '{}'", cmd.replace('\'', "'\\''"));
                    let call = serde_json::json!({
                        "command": wrapped_cmd,
                        "cancel_scope": mission_scope,
                    });
                    let call_text = call.to_string();
                    if !self.may_retry_node(run.idx) {
                        run.res = Err(EaiError::governance(format!(
                            "refuse replay of uncertain side effect for {node_id}"
                        )));
                        break;
                    }
                    // The operation id excludes the attempt-local cancellation
                    // scope.  A resumed node therefore addresses the same
                    // append/create/deploy-like effect even when its worker
                    // process receives a fresh scope.
                    let operation_key = format!("{node_id}:exec_command:{block_index}");
                    let intent_id = match self.prepare_dispatch_intent(
                        run.idx,
                        "exec_command",
                        cmd,
                        Some(&operation_key),
                    ) {
                        DispatchDecision::Dispatch { intent_id } => intent_id,
                        DispatchDecision::AlreadyExecuted { intent_id, receipt } => {
                            executed_scripts.push_str(&format!(
                                "\n\nExecution Receipt for `{cmd}`: already acknowledged (intent {intent_id}, output digest {})\n",
                                receipt.output_digest
                            ));
                            continue;
                        }
                        DispatchDecision::ReconciliationRequired { intent_id, reason } => {
                            run.res = Err(EaiError::governance(format!(
                                "refuse replay of side effect for {node_id} (intent {intent_id}): {reason}"
                            )));
                            break;
                        }
                    };
                    // Intent persistence is itself a mutating boundary: it
                    // is published only after the same authority check and
                    // before execute_tool is allowed to run.
                    if let Some(ctx) = persist_slot.as_mut() {
                        let leases = self.leases.clone();
                        let journal = self
                            .intent_journal
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let ownership = self.ownership;
                        ctx.mission.transaction(ctx.dir, |mission| {
                            mission.ownership = ownership;
                            mission.leases = leases;
                            mission.recovery = mission.leases.recovery.clone();
                            mission.side_effect_journal = journal;
                            Ok(())
                        })?;
                    }
                    let tool_call = call.clone();
                    let tool_workspace = node_ws.clone();
                    let tool_executor = Arc::clone(&tool_executor);
                    let result = match run_cancellable(&cancellation, move || {
                        tool_executor("exec_command", &tool_call, &tool_workspace)
                    }) {
                        Ok(CancellableResult::Completed(Ok(output))) => output,
                        Ok(CancellableResult::Completed(Err(error))) => {
                            format!("[Error] {error}")
                        }
                        Ok(CancellableResult::Cancelled)
                        | Ok(CancellableResult::DeadlineExceeded) => {
                            run.cancelled = true;
                            break;
                        }
                        Err(error) => format!("[Error] {error}"),
                    };
                    // A takeover that arrived while the tool was running
                    // cannot be folded or acknowledged by the old worker.
                    if self
                        .leases
                        .check_authority(&node_id, &run.owner, token, now_unix())
                        != CompleteVerdict::Accepted
                    {
                        run.stale_fence = true;
                        break;
                    }
                    if !result.starts_with("[Error]") {
                        self.mark_intent_executed_with_receipt(&intent_id, &result);
                        if let Some(ctx) = persist_slot.as_mut() {
                            let leases = self.leases.clone();
                            let journal = self
                                .intent_journal
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone();
                            let ownership = self.ownership;
                            ctx.mission.transaction(ctx.dir, |mission| {
                                mission.ownership = ownership;
                                mission.leases = leases;
                                mission.recovery = mission.leases.recovery.clone();
                                mission.side_effect_journal = journal;
                                Ok(())
                            })?;
                        }
                    }
                    node_calls.push(call_text);
                    executed_scripts
                        .push_str(&format!("\n\nExecution Result for `{cmd}`:\n{result}\n"));
                }
                run.calls = node_calls;
                if !executed_scripts.is_empty() {
                    if let Ok(output) = &mut run.res {
                        output.push_str(&executed_scripts);
                        if let Some(citations) =
                            crate::susi_core::capture::EvidenceSession::auto_format_truth(&node_ws)
                        {
                            output.push_str("\n\n");
                            output.push_str(&citations);
                        }
                    }
                }
            }

            // Fold each scope's writes back into the shared workspace,
            // sequentially and in node order — deterministic, reviewable
            // conflicts instead of silently lost writes.
            let mut write_conflicts: Vec<String> = Vec::new();
            let mut conflicted_nodes: BTreeSet<usize> = BTreeSet::new();
            let mut stale_nodes: Vec<usize> = Vec::new();
            for run in &batch_results {
                // Cancelled workers' private writes are discarded, never
                // folded — a killed command's partial output must not land.
                if run.cancelled {
                    isolation.cleanup(&run.scope);
                    continue;
                }
                // Re-check the fence before merging private writes
                // (T-DEVIN-9): a lease displaced mid-run means this worker's
                // writes are stale — discarding them is safe because they
                // never left the private scope.
                let run_stale = run.stale_fence
                    || self.leases.check_fence(
                        &Self::persist_id(run.idx),
                        &run.owner,
                        run.fence,
                        now_unix(),
                    ) != CompleteVerdict::Accepted;
                if run_stale {
                    stale_nodes.push(run.idx);
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
                            if !rest.cancelled && !stale_nodes.contains(&rest.idx) {
                                isolation.cleanup(&rest.scope);
                            }
                        }
                        return Err(e);
                    }
                }
            }
            if !stale_nodes.is_empty() {
                let detail = stale_nodes
                    .iter()
                    .map(|i| format!("n{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                if let Some(ctx) = persist_slot.as_mut() {
                    ctx.mission.leases = self.leases.clone();
                    ctx.mission.side_effect_journal = self
                        .intent_journal
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    for &idx in &stale_nodes {
                        let id = Self::persist_id(idx);
                        if let Some(node) = ctx.mission.nodes.get_mut(&id) {
                            node.state = NodeTerminal::Failed;
                            node.output =
                                Some("[STALE_FENCE] worker's fence was displaced".to_string());
                        }
                    }
                    let _ = ctx.mission.save(ctx.dir);
                }
                return Err(EaiError::governance(format!(
                    "DAG_EXECUTION_FAILED: stale fence rejected worker writes: {detail}"
                )));
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

            // The batch is now reconciled and its scopes are no longer live.
            // Clear the recovery marker atomically before publishing node
            // completions so a later restart cancels only genuinely active
            // workers.
            if let Some(ctx) = persist_slot.as_mut() {
                let leases = self.leases.clone();
                let journal = self
                    .intent_journal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let ownership = self.ownership;
                ctx.mission.transaction(ctx.dir, |mission| {
                    mission.ownership = ownership;
                    mission.leases = leases;
                    mission.clear_active_scope(&mission_scope);
                    mission.recovery = mission.leases.recovery.clone();
                    mission.side_effect_journal = journal;
                    Ok(())
                })?;
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
                            if self.nodes[idx].title == "Independent Verify" {
                                let conclusion = serde_json::from_str::<ReviewConclusion>(
                                    output.trim(),
                                )
                                .map_err(|error| {
                                    EaiError::governance(format!(
                                        "independent review output is not a conclusion: {error}"
                                    ))
                                })?;
                                if !self.accept_independent_verify(&conclusion) {
                                    return Err(EaiError::governance(
                                        "independent review receipt or reviewer binding rejected",
                                    ));
                                }
                            }
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
                                // Lease consumed by completion — persist the
                                // table so resume sees it closed (T-DEVIN-9).
                                ctx.mission.leases = self.leases.clone();
                                ctx.mission.ownership = self.ownership;
                                ctx.mission.recovery = self.leases.recovery.clone();
                                ctx.mission.side_effect_journal = self
                                    .intent_journal
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .clone();
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
            // Releasing the batch reservations after all completion records
            // are published wakes queued missions and drains the next fair
            // batch. Early returns above drop the same tokens on failure or
            // cancellation.
            drop(batch_reservations);
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
    dispatch_mission_dag_inner(
        goal,
        workspace,
        blackboard,
        production_model_generator(),
        production_tool_executor(),
    )
}

/// Hermetic production-entry seam for regression tests. It deliberately
/// retains the persisted mission loading and dispatch hook around the same
/// `execute_dag_persisted` path used by the daemon.
#[cfg(test)]
pub(crate) fn dispatch_mission_dag_with_backends(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
    model_generator: DagModelGenerator,
    tool_executor: DagToolExecutor,
) -> Vec<(String, String)> {
    dispatch_mission_dag_inner(goal, workspace, blackboard, model_generator, tool_executor)
}

fn dispatch_mission_dag_inner(
    goal: &str,
    workspace: &Path,
    blackboard: &MissionBlackboard,
    model_generator: DagModelGenerator,
    tool_executor: DagToolExecutor,
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

    // The resume point is explicit (T-DEEPSEEK-93): a loaded mission that
    // found nodes mid-flight says which interrupted nodes are re-dispatched
    // and which completed work is preserved — before anything re-runs, so
    // the report survives even a failing resume.
    let resumed_nodes: Vec<String> = persist
        .nodes
        .values()
        .filter(|n| n.state == NodeTerminal::Running)
        .map(|n| n.id.clone())
        .collect();
    let prior_interruptions = persist.interruptions.len();
    let preserved_nodes: Vec<String> = persist
        .nodes
        .values()
        .filter(|n| n.state == NodeTerminal::Completed)
        .map(|n| n.id.clone())
        .collect();
    if !resumed_nodes.is_empty() || prior_interruptions > 0 {
        results.push((
            "MissionDag".to_string(),
            format!(
                "[MISSION_RESUMED] mission {mission_id}: re-dispatched {} interrupted node(s) [{}]; {} completed node(s) preserved [{}]",
                resumed_nodes.len(),
                resumed_nodes.join(","),
                preserved_nodes.len(),
                preserved_nodes.join(",")
            ),
        ));
    }

    let (event_tx, _event_rx) = crate::susi_core::bus::create_swarm_bus();
    let mut persist_ctx = MissionPersistCtx {
        mission: &mut persist,
        dir: &persist_dir,
    };
    match dag.execute_dag_persisted_with_backends(
        workspace,
        blackboard,
        &event_tx,
        &mut persist_ctx,
        model_generator,
        tool_executor,
    ) {
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
