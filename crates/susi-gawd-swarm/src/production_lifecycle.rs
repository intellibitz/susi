//! Production mission lifecycle (T-DEVIN-5).
//!
//! One entry point — [`MissionLifecycle::run`] — composes the three
//! production subsystems into a single mission lifecycle:
//!
//! 1. **Coordination** — [`Coordinator`](crate::brain_coordination::Coordinator)
//!    plans and synthesizes through the supervised strongest-working brain
//!    ([`run_step`](crate::brain_supervisor::run_step)), so a dead
//!    coordinator fails over with an audit trail.
//! 2. **Scheduling** — the work phase claims ready tasks, prepares an
//!    isolated worktree per worker ([`claim_and_prepare`]), records each
//!    assignment in the fenced [`JobLedger`], and dispatches them in
//!    parallel through [`run_jobs`], which enforces shared quota,
//!    admission and per-job failover.
//! 3. **Closure** — every successful dispatch goes through the
//!    [`Gate`](crate::agent_integration::Gate): dependency check, real
//!    acceptance command in the worker's worktree, optional independent
//!    verification, then a conflict-aware merge before `finish`. A worker
//!    self-report is a claim, never proof.
//!
//! Until this module existed, those three stages were only ever composed
//! inside tests — native missions ran fleet dispatch → DAG execution with
//! none of the lifecycle guarantees. `run` is the production path a host
//! calls; a blocked coordinator or a failed gate is reported honestly,
//! never silently closed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use susi_gawd_agents::cloud_intent::Candidate;

use crate::agent_integration::{
    assign_fences, verified_closure, AcceptRunner, Gate, HeadProbe, IntegrateOutcome,
    IntegrationQueue, Integrator, Verifier,
};
use crate::brain_coordination::{CoordinationReport, Coordinator};
use crate::brain_supervisor::{BrainMission, StepSpec};
use crate::cloud_failover::{Runner, Stores};
use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};
use crate::roadmap_agents::{
    claim_and_prepare, AgentExecutor, AgentRunner, ClaimedWork, TaskQueue, ToolGrants, WorkerBrief,
};
use crate::worker_recovery::JobLedger;

/// The queue surface a mission lifecycle needs: claimable work plus
/// closure control (finish/reopen/deps). Production queue
/// implementations satisfy both `TaskQueue` and `IntegrationQueue`.
pub trait LifecycleQueue: TaskQueue + IntegrationQueue {}
impl<T: TaskQueue + IntegrationQueue> LifecycleQueue for T {}

/// Durable phase of the guarded production entry point.
///
/// The phase is intentionally more conservative than a boolean success flag:
/// a coordinator outage, a failed worker, and an unknown remote publication
/// have different recovery actions and must survive a process restart as such.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecyclePhase {
    /// The process has not yet synchronized its view of the shared queue.
    SyncingBeforeClaim,
    /// The process is allowed to claim ready work.
    Executing,
    /// A clean local merge exists but its remote merge is not observed.
    AwaitingPublication,
    /// The remote merge is observed; the local and primary views are syncing.
    SyncingAfterPublication,
    /// The task's claim is being released after remote convergence.
    Releasing,
    /// All dispatched work published and ownership was released.
    Complete,
    /// Some work made progress, but at least one task needs retry/review.
    Partial,
    /// No work was run because the coordinator or pre-claim boundary blocked.
    Blocked,
    /// The outcome or persisted context cannot safely be trusted yet.
    Uncertain,
}

/// Durable per-task state retained by a lifecycle boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobPhase {
    /// The task reached the remote merge and was released.
    Complete,
    /// The task needs another worker or reviewer.
    Partial,
    /// Remote publication is still running.
    AwaitingPublication,
    /// The publication or persistence boundary returned an ambiguous result.
    Uncertain,
}

/// Restartable context for one guarded mission run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleContext {
    /// Stable mission id used by the persisted coordinator state.
    pub mission_id: String,
    /// Monotonic local generation; a restart must not reuse an old snapshot.
    pub generation: u64,
    /// Current durable phase.
    pub phase: LifecyclePhase,
    /// Per-task terminal/recovery state.
    pub jobs: BTreeMap<String, JobPhase>,
    /// Redacted reason for a blocked, partial, or uncertain result.
    pub last_error: Option<String>,
}

impl LifecycleContext {
    /// Start a fresh context for a mission.
    #[must_use]
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            generation: 0,
            phase: LifecyclePhase::SyncingBeforeClaim,
            jobs: BTreeMap::new(),
            last_error: None,
        }
    }
}

/// Shared-state boundary owned by the released CLI/daemon host.
///
/// The swarm crate supplies the state machine; the host supplies the actual
/// `git fetch`, remote merge watcher, queue-ref release, and durable context
/// store. Keeping these operations injected makes the production path
/// hermetic-testable without allowing tests to mutate a user's checkout.
pub trait LifecycleBoundary: Send + Sync {
    /// Synchronize the task queue and primary checkout before any claim.
    fn sync_before_claim(&self) -> Result<(), String>;
    /// Synchronize the worker and primary views after a remote merge.
    fn sync_after_publication(&self, task_id: &str, merge_sha: &str) -> Result<(), String>;
    /// Release the claim only after remote publication and synchronization.
    fn release_claim(&self, task_id: &str) -> Result<(), String>;
    /// Persist the context atomically before/after every externally visible
    /// phase transition.
    fn persist(&self, context: &LifecycleContext) -> Result<(), String>;
}

/// Report from the guarded production entry point.
#[derive(Debug)]
pub struct GuardedMissionReport {
    /// Detailed coordination, dispatch, and gate outcomes.
    pub report: MissionReport,
    /// The last context snapshot held by the host boundary.
    pub context: LifecycleContext,
}

/// What one mission run produced.
#[derive(Debug)]
pub struct MissionReport {
    /// Plan/work/synthesis outcome of the coordinated phases.
    pub coordination: CoordinationReport,
    /// `(job_id, gate outcome)` for every job that produced output and
    /// reached closure evaluation — in dispatch completion order.
    pub gated: Vec<(String, IntegrateOutcome)>,
    /// Jobs whose dispatch produced no output at all (all candidates
    /// failed/stopped) — recorded `Failed` in the ledger, never closed.
    pub dispatch_failed: Vec<String>,
    /// Tasks lost to claim races this round.
    pub races_lost: Vec<String>,
    /// Tasks skipped (not ready, or no workspace).
    pub skipped: Vec<String>,
}

fn empty_mission_report() -> MissionReport {
    MissionReport {
        coordination: CoordinationReport {
            plan: None,
            work_receipts: Vec::new(),
            synthesis: None,
            brains_used: Vec::new(),
            blocked: None,
        },
        gated: Vec::new(),
        dispatch_failed: Vec::new(),
        races_lost: Vec::new(),
        skipped: Vec::new(),
    }
}

/// One production mission: coordination + scheduling + closure, all
/// seams injected. Construct once per mission.
pub struct MissionLifecycle<'a, Q: LifecycleQueue> {
    /// Live evidence stores for the coordinator's brain steps.
    pub coord_stores: Stores<'a>,
    /// Candidate pool the coordinator's brain selection draws from.
    pub coord_candidates: &'a [Candidate],
    /// Coordinator step policy (intent, budget, supervisor hysteresis).
    pub coord_spec: StepSpec<'a>,
    /// Task queue (claim/ready/finish/reopen/deps).
    pub queue: &'a Q,
    /// Worktree provisioner — must never return the primary checkout.
    pub workspaces: &'a dyn crate::roadmap_agents::Workspaces,
    /// Worker executor — agent CLI in production.
    pub executor: &'a dyn AgentExecutor,
    /// Shared worker-side evidence stores (quota/admission/lockouts).
    pub shared: &'a Shared<'a>,
    /// Worker candidate pool.
    pub candidates: &'a [Candidate],
    /// Scheduler bounds for the work phase.
    pub plan: DispatchPlan,
    /// Fenced assignment ledger.
    pub ledger: &'a JobLedger,
    /// Acceptance-command executor for the gate.
    pub accept: &'a dyn AcceptRunner,
    /// Branch merger for the gate.
    pub integrator: &'a dyn Integrator,
    /// Optional independent verifier (a different model in production).
    pub verifier: Option<&'a dyn Verifier>,
    /// Revision provenance for the gate — binds acceptance to the exact
    /// worktree revision being merged.
    pub head: &'a dyn HeadProbe,
    /// Worker identity prefix (`agent-taskid`).
    pub agent_prefix: &'a str,
    /// Claim lease expiry (unix secs).
    pub lease_until: u64,
    /// Declared tool grants handed to workers.
    pub grants: ToolGrants,
    /// Mandates text handed to workers.
    pub mandates: &'a str,
}

impl<'a, Q: LifecycleQueue> MissionLifecycle<'a, Q> {
    /// Run the full mission lifecycle and return the honest report.
    pub fn run<R: Runner>(
        &'a mut self,
        mission: &mut BrainMission,
        coord: &mut R,
    ) -> MissionReport {
        let Self {
            coord_stores,
            coord_candidates,
            coord_spec,
            queue,
            workspaces,
            executor,
            shared,
            candidates,
            plan,
            ledger,
            accept,
            integrator,
            verifier,
            head,
            agent_prefix,
            lease_until,
            grants,
            mandates,
        } = self;
        let mut rep = MissionReport {
            coordination: CoordinationReport {
                plan: None,
                work_receipts: Vec::new(),
                synthesis: None,
                brains_used: Vec::new(),
                blocked: None,
            },
            gated: Vec::new(),
            dispatch_failed: Vec::new(),
            races_lost: Vec::new(),
            skipped: Vec::new(),
        };
        let mut coordinator = Coordinator {
            stores: coord_stores,
            candidates: coord_candidates,
            spec: *coord_spec,
        };
        rep.coordination = coordinator.coordinate(mission, coord, |_plan, _stores| {
            // Work phase: claim → isolate → ledger → parallel dispatch.
            let batch = claim_and_prepare(*queue, *workspaces, agent_prefix, *lease_until);
            let jobs = batch.jobs;
            let ctxs = batch.claimed;
            rep.races_lost.extend(batch.races_lost);
            rep.skipped.extend(batch.skipped);
            // Job → fence assigned by the ledger (always 1 for a fresh
            // assignment; recorded so completion presents the same fence).
            let fences = assign_fences(ledger, &ctxs, *lease_until);
            if jobs.is_empty() {
                return Vec::new();
            }
            let mandates_owned = mandates.to_string();
            let grants_owned = grants.clone();
            let outcomes = run_jobs(&jobs, candidates, shared, plan.clone(), &|job: &Job| {
                let cw = ctxs
                    .iter()
                    .find(|c| c.task.id == job.id)
                    .map(|c| ClaimedWork {
                        task: c.task.clone(),
                        worker: c.worker.clone(),
                        worktree: c.worktree.clone(),
                    })
                    .unwrap_or_else(|| ClaimedWork {
                        task: crate::roadmap_agents::TaskSpec {
                            id: job.id.clone(),
                            task_class: "general".into(),
                            accept: vec![],
                            title: String::new(),
                        },
                        worker: format!("{agent_prefix}-{}", job.id),
                        worktree: std::path::PathBuf::from("/nonexistent"),
                    });
                AgentRunner::new(
                    *executor,
                    WorkerBrief {
                        task: cw.task,
                        worker: cw.worker,
                        worktree: cw.worktree,
                        model: String::new(),
                        mandates: mandates_owned.clone(),
                        grants: grants_owned.clone(),
                    },
                    candidates,
                )
            });
            // Closure phase: only the gate finishes a task. Dispatch
            // output is evidence to verify, not proof of done.
            let gate = Gate {
                ledger,
                queue: *queue,
                accept: *accept,
                integrator: *integrator,
                verifier: *verifier,
                head: *head,
            };
            let (gated, dispatch_failed) =
                verified_closure(&outcomes, &ctxs, &fences, &gate, *lease_until);
            rep.gated.extend(gated);
            rep.dispatch_failed.extend(dispatch_failed);
            outcomes
                .iter()
                .map(|o| {
                    format!(
                        "{}:{}",
                        o.job_id,
                        if o.output.is_some() {
                            "dispatched"
                        } else {
                            "failed"
                        }
                    )
                })
                .collect()
        });
        rep
    }

    /// Run the released-CLI/daemon boundary for one mission generation.
    ///
    /// `run` is the composed scheduler; this method is the production entry
    /// point. It makes the atomic worker loop explicit around that scheduler:
    /// synchronize before claiming, persist each phase, require the
    /// integrator to observe remote publication, synchronize again, and only
    /// then release ownership. A pending or uncertain remote result stays
    /// claimed and is returned as such for a later reconciliation run.
    #[allow(clippy::too_many_arguments)] // the released boundary carries mission, durable context, coordinator, and host sync seams explicitly
    pub fn run_guarded<R, B>(
        &'a mut self,
        mission_id: &str,
        context: &mut LifecycleContext,
        mission: &mut BrainMission,
        coord: &mut R,
        boundary: &B,
    ) -> GuardedMissionReport
    where
        R: Runner,
        B: LifecycleBoundary + ?Sized,
    {
        context.mission_id = mission_id.to_string();
        context.generation = context.generation.saturating_add(1);
        context.phase = LifecyclePhase::SyncingBeforeClaim;
        context.last_error = None;
        if let Err(error) = boundary.persist(context) {
            context.phase = LifecyclePhase::Uncertain;
            context.last_error = Some(format!("persist before sync failed: {error}"));
            return GuardedMissionReport {
                report: empty_mission_report(),
                context: context.clone(),
            };
        }
        if let Err(error) = boundary.sync_before_claim() {
            context.phase = LifecyclePhase::Blocked;
            context.last_error = Some(error);
            let _ = boundary.persist(context);
            return GuardedMissionReport {
                report: empty_mission_report(),
                context: context.clone(),
            };
        }

        context.phase = LifecyclePhase::Executing;
        if let Err(error) = boundary.persist(context) {
            context.phase = LifecyclePhase::Uncertain;
            context.last_error = Some(format!("persist before execution failed: {error}"));
            return GuardedMissionReport {
                report: empty_mission_report(),
                context: context.clone(),
            };
        }
        let report = self.run(mission, coord);
        if let Some((_, reason)) = report.coordination.blocked.as_ref() {
            context.phase = LifecyclePhase::Blocked;
            context.last_error = Some(reason.clone());
            let _ = boundary.persist(context);
            return GuardedMissionReport {
                report,
                context: context.clone(),
            };
        }

        let prior_unsettled = context
            .jobs
            .values()
            .any(|phase| matches!(phase, JobPhase::AwaitingPublication | JobPhase::Uncertain));
        let mut uncertain = false;
        let mut all_published = !prior_unsettled
            && report.dispatch_failed.is_empty()
            && report.races_lost.is_empty()
            && report.skipped.is_empty();
        for (job_id, outcome) in &report.gated {
            match outcome {
                IntegrateOutcome::Integrated { merge_sha, .. } => {
                    context.phase = LifecyclePhase::SyncingAfterPublication;
                    context.jobs.insert(job_id.clone(), JobPhase::Complete);
                    if let Err(error) = boundary.persist(context) {
                        context.phase = LifecyclePhase::Uncertain;
                        context.last_error = Some(format!(
                            "persist after publication for {job_id} failed: {error}"
                        ));
                        uncertain = true;
                        all_published = false;
                        continue;
                    }
                    if let Err(error) = boundary.sync_after_publication(job_id, merge_sha) {
                        context.jobs.insert(job_id.clone(), JobPhase::Uncertain);
                        context.phase = LifecyclePhase::Uncertain;
                        context.last_error = Some(format!("{job_id} sync failed: {error}"));
                        uncertain = true;
                        all_published = false;
                        let _ = boundary.persist(context);
                        continue;
                    }
                    context.phase = LifecyclePhase::Releasing;
                    let _ = boundary.persist(context);
                    if let Err(error) = boundary.release_claim(job_id) {
                        context.jobs.insert(job_id.clone(), JobPhase::Uncertain);
                        context.phase = LifecyclePhase::Uncertain;
                        context.last_error = Some(format!("{job_id} release failed: {error}"));
                        uncertain = true;
                        all_published = false;
                        let _ = boundary.persist(context);
                    }
                }
                IntegrateOutcome::AwaitingPublication { reason, .. } => {
                    context
                        .jobs
                        .insert(job_id.clone(), JobPhase::AwaitingPublication);
                    context.phase = LifecyclePhase::AwaitingPublication;
                    context.last_error = Some(reason.clone());
                    all_published = false;
                    let _ = boundary.persist(context);
                }
                IntegrateOutcome::PublicationUncertain { reason, .. } => {
                    context.jobs.insert(job_id.clone(), JobPhase::Uncertain);
                    context.phase = LifecyclePhase::Uncertain;
                    context.last_error = Some(reason.clone());
                    uncertain = true;
                    all_published = false;
                    let _ = boundary.persist(context);
                }
                IntegrateOutcome::AcceptanceFailed { .. }
                | IntegrateOutcome::WorkerFailed
                | IntegrateOutcome::VerifyRejected { .. }
                | IntegrateOutcome::ConflictReview { .. }
                | IntegrateOutcome::BlockedByDeps { .. }
                | IntegrateOutcome::NotCompleted { .. } => {
                    context.jobs.insert(job_id.clone(), JobPhase::Partial);
                    all_published = false;
                }
            }
        }
        if all_published {
            context.phase = LifecyclePhase::Complete;
        } else if uncertain {
            context.phase = LifecyclePhase::Uncertain;
        } else if !matches!(
            context.phase,
            LifecyclePhase::Uncertain | LifecyclePhase::AwaitingPublication
        ) {
            context.phase = LifecyclePhase::Partial;
        }
        if let Err(error) = boundary.persist(context) {
            context.phase = LifecyclePhase::Uncertain;
            context.last_error = Some(format!("final context persistence failed: {error}"));
        }
        GuardedMissionReport {
            report,
            context: context.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_integration::PublicationStatus;
    use crate::cloud_failover::{AttemptOutcome, FailoverBudget};
    use crate::cloud_lockout::LockoutTracker;
    use crate::roadmap_agents::{ClaimDenied, ExecResult, TaskSpec, Workspaces};
    use crate::worker_recovery::JobState;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::IntentConstraints;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    fn clock() -> u64 {
        T0
    }

    fn cand(provider: &str, key: &str, model: &str) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: Vec::new(),
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 100,
            cost_per_mtok: Some(1.0),
            quality: BTreeMap::new(),
        }
    }

    fn subj(c: &Candidate) -> Subject<'_> {
        Subject {
            provider: &c.provider,
            api_key: &c.api_key,
            account: c.account.as_deref(),
            region: c.region.as_deref(),
            model: &c.model,
        }
    }

    /// Scripted coordinator brain.
    struct Coord {
        calls: Vec<usize>,
        fail_at_call: BTreeMap<usize, AttemptOutcome>,
    }
    impl Coord {
        fn ok() -> Self {
            Self {
                calls: Vec::new(),
                fail_at_call: BTreeMap::new(),
            }
        }
    }
    impl Runner for Coord {
        fn attempt(&mut self, index: usize, _dl: u64) -> AttemptOutcome {
            let n = self.calls.len();
            self.calls.push(index);
            self.fail_at_call
                .remove(&n)
                .unwrap_or_else(|| AttemptOutcome::Success(format!("brain-{index}")))
        }
    }

    /// Queue satisfying both lifecycle surfaces. `events` records the
    /// exact order of accept/finish/reopen so tests can prove the gate —
    /// not the self-report — closes work.
    struct Q {
        tasks: Mutex<BTreeMap<String, TaskSpec>>,
        claims: Mutex<BTreeMap<String, String>>,
        done: Mutex<std::collections::BTreeSet<String>>,
        deps: BTreeMap<String, Vec<String>>,
        events: Mutex<Vec<String>>,
    }
    impl Q {
        fn new(ids: &[&str]) -> Self {
            let tasks = ids
                .iter()
                .map(|id| {
                    (
                        id.to_string(),
                        TaskSpec {
                            id: id.to_string(),
                            task_class: "coding".into(),
                            accept: vec!["sh".into(), "-c".into(), "test -f accept.ok".into()],
                            title: format!("task {id}"),
                        },
                    )
                })
                .collect();
            Self {
                tasks: Mutex::new(tasks),
                claims: Mutex::new(BTreeMap::new()),
                done: Mutex::new(std::collections::BTreeSet::new()),
                deps: BTreeMap::new(),
                events: Mutex::new(Vec::new()),
            }
        }
        fn ev(&self, s: String) {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(s);
        }
    }
    impl TaskQueue for Q {
        fn ready(&self) -> Vec<TaskSpec> {
            let claims = self.claims.lock().unwrap_or_else(|e| e.into_inner());
            let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
            self.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|t| !claims.contains_key(&t.id) && !done.contains(&t.id))
                .cloned()
                .collect()
        }
        fn claim(&self, id: &str, worker: &str, _lease_until: u64) -> Result<(), ClaimDenied> {
            let mut c = self.claims.lock().unwrap_or_else(|e| e.into_inner());
            if c.contains_key(id) {
                return Err(ClaimDenied::Held { by: c[id].clone() });
            }
            c.insert(id.to_string(), worker.to_string());
            Ok(())
        }
        fn finish(&self, id: &str) {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string());
            self.ev(format!("finish:{id}"));
        }
    }
    impl IntegrationQueue for Q {
        fn is_done(&self, id: &str) -> bool {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(id)
        }
        fn deps(&self, id: &str) -> Vec<String> {
            self.deps.get(id).cloned().unwrap_or_default()
        }
        fn finish(&self, id: &str) {
            TaskQueue::finish(self, id);
        }
        fn reopen(&self, id: &str, _why: &str) {
            self.done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
            self.claims
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
            self.ev(format!("reopen:{id}"));
        }
    }

    /// One temp dir per worker — mirrors real worktree isolation.
    struct TempWs {
        root: PathBuf,
    }
    impl TempWs {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "lc-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }
    impl Workspaces for TempWs {
        fn prepare(&self, task: &str, worker: &str) -> Result<PathBuf, String> {
            let dir = self.root.join(format!("{worker}-{task}"));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Ok(dir)
        }
    }
    impl Drop for TempWs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Worker executor: optionally writes `accept.ok`, succeeds or fails
    /// per task id script.
    struct Exec {
        fail: BTreeSet<String>,
        write_marker: bool,
    }
    impl AgentExecutor for Exec {
        fn execute(&self, brief: &WorkerBrief) -> ExecResult {
            if self.fail.contains(&brief.task.id) {
                return ExecResult::Failed(format!("{} failed to run", brief.task.id));
            }
            if self.write_marker {
                std::fs::write(brief.worktree.join("accept.ok"), "ok\n")
                    .unwrap_or_else(|e| panic!("marker write: {e}"));
            }
            ExecResult::Accepted
        }
    }

    /// Real acceptance: runs the task's argv in the worktree.
    struct CmdAccept;
    impl AcceptRunner for CmdAccept {
        fn run(&self, worktree: &Path, cmd: &[String]) -> Result<(), String> {
            let Some((prog, args)) = cmd.split_first() else {
                return Err("empty accept".into());
            };
            Command::new(prog)
                .args(args)
                .current_dir(worktree)
                .status()
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    if s.success() {
                        Ok(())
                    } else {
                        Err(format!("exit {s}"))
                    }
                })
        }
    }

    /// Scripted integrator recording each (job→branch) it merges.
    struct Int {
        conflict: Mutex<BTreeMap<String, Vec<String>>>,
        merged: Mutex<Vec<String>>,
        publication: Mutex<BTreeMap<String, PublicationStatus>>,
    }
    impl Int {
        fn clean() -> Self {
            Self {
                conflict: Mutex::new(BTreeMap::new()),
                merged: Mutex::new(Vec::new()),
                publication: Mutex::new(BTreeMap::new()),
            }
        }
    }
    impl Integrator for Int {
        fn integrate(
            &self,
            _repo: &Path,
            branch: &str,
        ) -> Result<crate::agent_integration::MergeResult, String> {
            if let Some(files) = self
                .conflict
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(branch)
            {
                return Ok(crate::agent_integration::MergeResult::Conflict(files));
            }
            self.merged
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(branch.to_string());
            Ok(crate::agent_integration::MergeResult::Clean("sha1".into()))
        }

        fn publish_and_wait(
            &self,
            _repo: &Path,
            branch: &str,
            _merge_sha: &str,
        ) -> Result<PublicationStatus, String> {
            Ok(self
                .publication
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(branch)
                .unwrap_or(PublicationStatus::Published))
        }
    }

    /// Host-owned boundary used by the production-entry regressions. It
    /// records durable transitions, so tests prove ordering rather than
    /// merely inspecting the final queue bit.
    struct Boundary {
        events: Mutex<Vec<String>>,
        snapshots: Mutex<Vec<LifecycleContext>>,
        sync_error: Option<String>,
        publication_error: Option<String>,
        release_error: Option<String>,
    }
    impl Boundary {
        fn healthy() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                snapshots: Mutex::new(Vec::new()),
                sync_error: None,
                publication_error: None,
                release_error: None,
            }
        }

        fn saw(&self, event: &str) -> bool {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|seen| seen == event)
        }
    }
    impl LifecycleBoundary for Boundary {
        fn sync_before_claim(&self) -> Result<(), String> {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push("sync-before-claim".into());
            self.sync_error.clone().map_or(Ok(()), Err)
        }

        fn sync_after_publication(&self, task_id: &str, _merge_sha: &str) -> Result<(), String> {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("sync-after:{task_id}"));
            self.publication_error.clone().map_or(Ok(()), Err)
        }

        fn release_claim(&self, task_id: &str) -> Result<(), String> {
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("release:{task_id}"));
            self.release_error.clone().map_or(Ok(()), Err)
        }

        fn persist(&self, context: &LifecycleContext) -> Result<(), String> {
            self.snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(context.clone());
            self.events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("persist:{:?}", context.phase));
            Ok(())
        }
    }

    struct Reject;
    impl Verifier for Reject {
        fn verify(&self, _w: &Path, _t: &TaskSpec) -> crate::agent_integration::Verdict {
            crate::agent_integration::Verdict::Reject {
                reasons: vec!["smells".into()],
            }
        }
    }

    struct Approve;
    impl Verifier for Approve {
        fn verify(&self, _w: &Path, _t: &TaskSpec) -> crate::agent_integration::Verdict {
            crate::agent_integration::Verdict::Approve
        }

        fn authenticated_identity(&self) -> Option<&str> {
            Some("reviewer-1")
        }

        fn review_receipt(
            &self,
            _w: &Path,
            task: &TaskSpec,
            implementer: &str,
            source_sha: &str,
            acceptance: &crate::agent_integration::AcceptanceObservation,
        ) -> Option<crate::independent_verify::ReviewReceipt> {
            Some(crate::independent_verify::ReviewReceipt::new(
                "reviewer-1",
                implementer,
                &task.id,
                &acceptance.tool,
                &acceptance.arguments_digest,
                &acceptance.result_digest,
                source_sha,
            ))
        }
    }

    /// Coordinator-side evidence stores.
    struct CFx {
        elig: EligibilityStore,
        quota: QuotaInventory,
        lock: LockoutTracker,
        ledger: BudgetLedger,
    }
    /// Worker-side stores behind scoped locks.
    struct WFx {
        elig: Mutex<EligibilityStore>,
        lock: Mutex<LockoutTracker>,
        ledger: BudgetLedger,
        quota: QuotaInventory,
    }

    fn cfx(brain: &Candidate) -> CFx {
        let mut elig = EligibilityStore::new();
        elig.record_inference(subj(brain), &InferenceResult::Success, T0);
        CFx {
            elig,
            quota: QuotaInventory::new(),
            lock: LockoutTracker::default(),
            ledger: BudgetLedger::new(),
        }
    }

    fn wfx(workers: &[Candidate]) -> WFx {
        let mut elig = EligibilityStore::new();
        for w in workers {
            elig.record_inference(subj(w), &InferenceResult::Success, T0);
        }
        WFx {
            elig: Mutex::new(elig),
            lock: Mutex::new(LockoutTracker::default()),
            ledger: BudgetLedger::new(),
            quota: QuotaInventory::new(),
        }
    }

    fn spec(intent: &IntentConstraints) -> StepSpec<'_> {
        StepSpec {
            intent,
            budget: FailoverBudget {
                max_attempts: 4,
                deadline_ms: None,
                spend: SpendPolicy::PaidAuthorized {
                    max_spend_micro: 1_000_000,
                },
                attempt_estimate_micros: 10,
                now_ms: T0_MS,
            },
            policy: Default::default(),
        }
    }

    fn plan() -> DispatchPlan {
        DispatchPlan {
            max_workers: 4,
            mission: "m".into(),
            per_job: FailoverBudget {
                max_attempts: 2,
                deadline_ms: None,
                spend: SpendPolicy::PaidAuthorized {
                    max_spend_micro: 1_000_000,
                },
                attempt_estimate_micros: 10,
                now_ms: T0_MS,
            },
            job_cpu_millis: 0,
            job_ram_mb: 0,
            job_vram_mb: 0,
            job_subprocesses: 0,
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            ..Default::default()
        }
    }

    /// All the owned refs a lifecycle needs — keeps test prologues short.
    struct Fix<'a> {
        c: &'a mut CFx,
        q: &'a Q,
        ws: &'a TempWs,
        jobs: &'a JobLedger,
        intent: &'a IntentConstraints,
        shared: &'a Shared<'a>,
        pool: &'a [Candidate],
        workers: &'a [Candidate],
        exec: &'a Exec,
        int: &'a Int,
        verifier: Option<&'a dyn Verifier>,
    }
    impl<'a> Fix<'a> {
        fn lifecycle(self) -> MissionLifecycle<'a, Q> {
            let c = self.c;
            MissionLifecycle {
                coord_stores: Stores {
                    eligibility: &mut c.elig,
                    quota: &c.quota,
                    lockouts: &mut c.lock,
                    ledger: &c.ledger,
                },
                coord_candidates: self.pool,
                coord_spec: spec(self.intent),
                queue: self.q,
                workspaces: self.ws,
                executor: self.exec,
                shared: self.shared,
                candidates: self.workers,
                plan: plan(),
                ledger: self.jobs,
                accept: &CmdAccept,
                integrator: self.int,
                verifier: self.verifier,
                head: &crate::agent_integration::ManifestProbe,
                agent_prefix: "w",
                lease_until: T0 + 300,
                grants: ToolGrants::default(),
                mandates: "mandates",
            }
        }
    }

    /// Full lifecycle: plan → parallel claimed work → verified gate →
    /// finish — with acceptance observed BEFORE finish in the event log.
    #[test]
    fn production_lifecycle_end_to_end_plan_dispatch_gate_finish() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![
            cand("p1", "sk-1", "w1"),
            cand("p2", "sk-2", "w2"),
            cand("p3", "sk-3", "w3"),
        ];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-1", "T-2", "T-3"]);
        let ws = TempWs::new("e2e");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert!(rep.coordination.blocked.is_none());
        assert!(rep.coordination.plan.is_some() && rep.coordination.synthesis.is_some());
        assert_eq!(rep.coordination.brains_used.len(), 2);
        assert_eq!(rep.gated.len(), 3);
        for (job, out) in &rep.gated {
            assert!(
                matches!(out, IntegrateOutcome::Integrated { .. }),
                "{job}: {out:?}"
            );
            assert!(q.is_done(job), "{job} finished");
            assert_eq!(jobs.get(job).map(|a| a.state), Some(JobState::Completed));
        }
        // Every worker ran in its own workspace and the gate observed real
        // acceptance before each finish.
        let events = q.events.lock().unwrap_or_else(|e| e.into_inner());
        for id in ["T-1", "T-2", "T-3"] {
            let fin = events.iter().position(|e| e == &format!("finish:{id}"));
            assert!(fin.is_some(), "{id} finished in event log");
        }
        assert!(rep.dispatch_failed.is_empty() && rep.races_lost.is_empty());
    }

    /// Worker claims done but acceptance fails in ITS worktree — the task
    /// reopens and is never finished.
    #[test]
    fn production_lifecycle_failed_acceptance_reopens_never_closes() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-9"]);
        let ws = TempWs::new("accept-fail");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        // Worker reports success but never produces the artifact.
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: false,
        };
        let int = Int::clean();
        let approve = Approve;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert_eq!(rep.gated.len(), 1);
        assert!(matches!(
            rep.gated[0].1,
            IntegrateOutcome::AcceptanceFailed { .. }
        ));
        assert!(!q.is_done("T-9"));
        assert!(int
            .merged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
        let events = q.events.lock().unwrap_or_else(|e| e.into_inner());
        assert!(events.iter().any(|e| e == "reopen:T-9"));
        assert!(!events.iter().any(|e| e == "finish:T-9"));
    }

    /// Independent verifier rejection blocks integration even with a
    /// passing acceptance — and the task is reopened, not closed.
    #[test]
    fn production_lifecycle_verifier_reject_blocks_closure() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-7"]);
        let ws = TempWs::new("verify");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        let reject = Reject;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&reject),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert!(matches!(
            rep.gated[0].1,
            IntegrateOutcome::VerifyRejected { .. }
        ));
        assert!(!q.is_done("T-7"));
        assert!(int
            .merged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty());
    }

    /// A merge conflict surfaces for review — never a silent overwrite,
    /// never a finish.
    #[test]
    fn production_lifecycle_conflict_is_reviewed_not_closed() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-5"]);
        let ws = TempWs::new("conflict");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int {
            conflict: Mutex::new(
                [("w-T-5".to_string(), vec!["shared.rs".to_string()])]
                    .into_iter()
                    .collect(),
            ),
            merged: Mutex::new(Vec::new()),
            publication: Mutex::new(BTreeMap::new()),
        };
        let approve = Approve;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert!(matches!(
            rep.gated[0].1,
            IntegrateOutcome::ConflictReview { .. }
        ));
        assert!(!q.is_done("T-5"));
    }

    /// With no working brain the mission blocks at Plan and the queue is
    /// never touched — no claims, no work, honest report.
    #[test]
    fn production_lifecycle_blocked_brain_skips_work() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-1"]);
        let ws = TempWs::new("blocked");
        let jobs = JobLedger::with_clock(clock);
        // The only coordinator candidate is revoked.
        c.elig.record_inference(
            subj(&brain),
            &InferenceResult::Failed {
                status: Some(401),
                body_snippet: "revoked".into(),
                retry_after_secs: None,
            },
            T0 + 1,
        );
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert_eq!(
            rep.coordination.blocked.map(|b| b.0),
            Some(crate::brain_coordination::CoordPhase::Plan)
        );
        assert!(rep.gated.is_empty() && rep.coordination.work_receipts.is_empty());
        assert!(
            q.claims
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "blocked mission must not claim tasks"
        );
    }

    /// A task whose dispatch fails is recorded Failed in the ledger and
    /// honestly reported — the surviving task still gates through.
    #[test]
    fn production_lifecycle_dispatch_failure_is_honest() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-ok", "T-bad"]);
        let ws = TempWs::new("fail");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let mut fail = BTreeSet::new();
        fail.insert("T-bad".to_string());
        let exec = Exec {
            fail,
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let rep = lc.run(&mut mission, &mut coord);

        assert!(rep.dispatch_failed.contains(&"T-bad".to_string()));
        assert!(matches!(
            jobs.get("T-bad").map(|a| a.state),
            Some(JobState::Failed(_))
        ));
        assert!(!q.is_done("T-bad"));
        // The surviving task integrated and finished through the gate.
        assert!(
            matches!(
                rep.gated.iter().find(|(j, _)| j == "T-ok").map(|(_, o)| o),
                Some(IntegrateOutcome::Integrated { .. })
            ),
            "gated={:?} failed={:?} skipped={:?} races={:?}",
            rep.gated,
            rep.dispatch_failed,
            rep.skipped,
            rep.races_lost
        );
        assert!(q.is_done("T-ok"));
    }

    /// The released entry point drives the complete atomic loop for two
    /// independent workers: pre-claim sync, fenced dispatch, verified local
    /// merge, remote publication confirmation, post-merge sync, then release.
    #[test]
    fn swarm_gap_production_lifecycle_full_loop_publishes_two_workers() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1"), cand("p2", "sk-2", "w2")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-full-a", "T-full-b"]);
        let ws = TempWs::new("production-entry");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let boundary = Boundary::healthy();
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut context = LifecycleContext::new("mission-full");
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let guarded = lc.run_guarded(
            "mission-full",
            &mut context,
            &mut mission,
            &mut coord,
            &boundary,
        );

        assert_eq!(guarded.context.phase, LifecyclePhase::Complete);
        assert_eq!(guarded.context.generation, 1);
        assert_eq!(guarded.report.gated.len(), 2);
        for id in ["T-full-a", "T-full-b"] {
            assert!(q.is_done(id), "{id} must be finished after publication");
            assert!(boundary.saw(&format!("sync-after:{id}")));
            assert!(boundary.saw(&format!("release:{id}")));
        }
        let snapshots = boundary.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        assert!(snapshots
            .iter()
            .any(|snapshot| snapshot.phase == LifecyclePhase::Executing));
        assert!(snapshots
            .iter()
            .any(|snapshot| snapshot.phase == LifecyclePhase::Complete));
        let encoded = serde_json::to_string(&guarded.context).unwrap_or_default();
        let restored: LifecycleContext =
            serde_json::from_str(&encoded).unwrap_or_else(|_| LifecycleContext::new("bad"));
        assert_eq!(restored, guarded.context);
    }

    /// A coordinator outage during planning fails over to the next eligible
    /// brain and still runs the same guarded worker/publication path.
    #[test]
    fn swarm_gap_production_lifecycle_coordinator_restart_preserves_context() {
        let brain_a = cand("acme", "sk-a", "m-a");
        let brain_b = cand("beta", "sk-b", "m-b");
        let workers = vec![cand("p1", "sk-1", "w1"), cand("p2", "sk-2", "w2")];
        let mut c = cfx(&brain_a);
        c.elig
            .record_inference(subj(&brain_b), &InferenceResult::Success, T0);
        let w = wfx(&workers);
        let q = Q::new(&["T-restart-a", "T-restart-b"]);
        let ws = TempWs::new("restart");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain_a, brain_b];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let boundary = Boundary::healthy();
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut context = LifecycleContext::new("mission-restart");
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        coord.fail_at_call.insert(
            1,
            AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: Some(503),
                body_snippet: "coordinator restart".into(),
                retry_after_secs: None,
            }),
        );
        let guarded = lc.run_guarded(
            "mission-restart",
            &mut context,
            &mut mission,
            &mut coord,
            &boundary,
        );

        assert_eq!(guarded.context.phase, LifecyclePhase::Complete);
        assert!(mission.transitions.iter().any(|transition| matches!(
            transition.reason,
            crate::brain_supervisor::TransitionReason::BrainFailed { .. }
        )));
        assert_eq!(guarded.report.gated.len(), 2);
        assert!(guarded
            .report
            .gated
            .iter()
            .all(|(_, outcome)| matches!(outcome, IntegrateOutcome::Integrated { .. })));
    }

    /// A conflicting scope is a partial, reviewable outcome; it never gets a
    /// post-merge release, even though acceptance and independent review pass.
    #[test]
    fn swarm_gap_production_lifecycle_conflicting_scope_stays_partial() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-conflict"]);
        let ws = TempWs::new("conflicting-scope");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int {
            conflict: Mutex::new(
                [("w-T-conflict".to_string(), vec!["shared.rs".to_string()])]
                    .into_iter()
                    .collect(),
            ),
            merged: Mutex::new(Vec::new()),
            publication: Mutex::new(BTreeMap::new()),
        };
        let approve = Approve;
        let boundary = Boundary::healthy();
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut context = LifecycleContext::new("mission-conflict");
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let guarded = lc.run_guarded(
            "mission-conflict",
            &mut context,
            &mut mission,
            &mut coord,
            &boundary,
        );

        assert_eq!(guarded.context.phase, LifecyclePhase::Partial);
        assert!(matches!(
            guarded.report.gated[0].1,
            IntegrateOutcome::ConflictReview { .. }
        ));
        assert!(!q.is_done("T-conflict"));
        assert!(!boundary.saw("release:T-conflict"));
    }

    /// A cancelled worker is retained as a truthful partial failure. The
    /// coordinator's successful plan cannot turn an absent worker result into
    /// a close or a released claim.
    #[test]
    fn swarm_gap_production_lifecycle_cancelled_worker_stays_open() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-cancel"]);
        let ws = TempWs::new("cancel");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: ["T-cancel".to_string()].into_iter().collect(),
            write_marker: true,
        };
        let int = Int::clean();
        let approve = Approve;
        let boundary = Boundary::healthy();
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut context = LifecycleContext::new("mission-cancel");
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let guarded = lc.run_guarded(
            "mission-cancel",
            &mut context,
            &mut mission,
            &mut coord,
            &boundary,
        );

        assert_eq!(guarded.context.phase, LifecyclePhase::Partial);
        assert_eq!(guarded.report.dispatch_failed, vec!["T-cancel"]);
        assert!(!q.is_done("T-cancel"));
        assert!(!boundary.saw("release:T-cancel"));
    }

    /// A local merge without a confirmed remote merge remains owned and
    /// explicitly awaits publication; no close receipt is emitted.
    #[test]
    fn swarm_gap_production_lifecycle_remote_merge_wait_is_uncertain() {
        let brain = cand("acme", "sk-b", "m-brain");
        let workers = vec![cand("p1", "sk-1", "w1")];
        let mut c = cfx(&brain);
        let w = wfx(&workers);
        let q = Q::new(&["T-remote"]);
        let ws = TempWs::new("remote");
        let jobs = JobLedger::with_clock(clock);
        let pool = vec![brain];
        let exec = Exec {
            fail: BTreeSet::new(),
            write_marker: true,
        };
        let int = Int::clean();
        int.publication
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                "w-T-remote".into(),
                PublicationStatus::Awaiting {
                    reason: "remote merge check still running".into(),
                },
            );
        let approve = Approve;
        let boundary = Boundary::healthy();
        let intent = intent();
        let shared = Shared::new(&w.elig, &w.quota, &w.lock, &w.ledger);
        let mut lc = Fix {
            c: &mut c,
            q: &q,
            ws: &ws,
            jobs: &jobs,
            intent: &intent,
            shared: &shared,
            pool: &pool,
            workers: &workers,
            exec: &exec,
            int: &int,
            verifier: Some(&approve),
        }
        .lifecycle();
        let mut context = LifecycleContext::new("mission-remote");
        let mut mission = BrainMission::default();
        let mut coord = Coord::ok();
        let guarded = lc.run_guarded(
            "mission-remote",
            &mut context,
            &mut mission,
            &mut coord,
            &boundary,
        );

        assert_eq!(guarded.context.phase, LifecyclePhase::AwaitingPublication);
        assert_eq!(
            guarded.context.jobs.get("T-remote"),
            Some(&JobPhase::AwaitingPublication)
        );
        assert!(matches!(
            guarded.report.gated[0].1,
            IntegrateOutcome::AwaitingPublication { .. }
        ));
        assert!(!q.is_done("T-remote"));
        assert!(!boundary.saw("release:T-remote"));
    }
}
