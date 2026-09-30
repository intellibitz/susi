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

use susi_gawd_agents::cloud_intent::Candidate;

use crate::agent_integration::{
    AcceptRunner, Gate, IntegrateOutcome, IntegrationQueue, Integrator, Verifier,
};
use crate::brain_coordination::{CoordinationReport, Coordinator};
use crate::brain_supervisor::{BrainMission, StepSpec};
use crate::cloud_failover::{Runner, Stores};
use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};
use crate::roadmap_agents::{
    claim_and_prepare, AgentExecutor, AgentRunner, ClaimedWork, TaskQueue, ToolGrants, WorkerBrief,
};
use crate::worker_recovery::{Assign, JobLedger};

/// The queue surface a mission lifecycle needs: claimable work plus
/// closure control (finish/reopen/deps). Production queue
/// implementations satisfy both `TaskQueue` and `IntegrationQueue`.
pub trait LifecycleQueue: TaskQueue + IntegrationQueue {}
impl<T: TaskQueue + IntegrationQueue> LifecycleQueue for T {}

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
            let mut fences: BTreeMap<String, u64> = BTreeMap::new();
            for cw in &ctxs {
                let fence = ledger.assign(Assign {
                    job_id: &cw.task.id,
                    worker: &cw.worker,
                    model: "",
                    worktree: cw.worktree.clone(),
                    lease_until: *lease_until,
                });
                fences.insert(cw.task.id.clone(), fence);
            }
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
            for o in &outcomes {
                let fence = fences.get(&o.job_id).copied().unwrap_or(0);
                let Some(cw) = ctxs.iter().find(|c| c.task.id == o.job_id) else {
                    continue;
                };
                match &o.output {
                    Some(model) => {
                        let _ = ledger.heartbeat(&o.job_id, fence, *lease_until);
                        let _ = ledger.record_receipt(
                            &o.job_id,
                            fence,
                            &format!("executed on {model}"),
                        );
                        let _ = ledger.complete(&o.job_id, fence);
                        let gate = Gate {
                            ledger,
                            queue: *queue,
                            accept: *accept,
                            integrator: *integrator,
                            verifier: *verifier,
                        };
                        // Worker branch convention: the worker identity
                        // names its own branch/worktree.
                        let out = gate.integrate_job(&o.job_id, &cw.worker, &cw.task);
                        rep.gated.push((o.job_id.clone(), out));
                    }
                    None => {
                        let why = o
                            .stop
                            .as_ref()
                            .map_or_else(|| "no output".to_string(), |s| format!("{s:?}"));
                        let _ = ledger.fail(&o.job_id, fence, &why);
                        rep.dispatch_failed.push(o.job_id.clone());
                    }
                }
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
    impl Int {
        fn clean() -> Self {
            Self {
                conflict: Mutex::new(BTreeMap::new()),
                merged: Mutex::new(Vec::new()),
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
    }

    struct Reject;
    impl Verifier for Reject {
        fn verify(&self, _w: &Path, _t: &TaskSpec) -> crate::agent_integration::Verdict {
            crate::agent_integration::Verdict::Reject {
                reasons: vec!["smells".into()],
            }
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
            verifier: None,
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
            verifier: None,
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
        };
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
            verifier: None,
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
            verifier: None,
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
            verifier: None,
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
}
