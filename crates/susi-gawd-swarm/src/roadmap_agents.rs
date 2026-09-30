//! Roadmap agents: connect the `susi tasks` queue to the multi-model
//! worker scheduler (`parallel_dispatch`).
//!
//! Flow per scheduling round:
//! 1. `TaskQueue::ready` — open tasks whose dependencies are done and which
//!    hold no live claim.
//! 2. Atomic `claim` with a distinct worker identity + lease — a lost race
//!    is skipped, never stolen (Mandate 50).
//! 3. `Workspaces::prepare` — every worker gets its OWN branch/worktree off
//!    the current main; never the primary checkout (Mandate 49).
//! 4. Dispatch via `run_jobs`: different tasks land on different working
//!    models simultaneously; per-job failover still applies.
//!
//! Workers are handed the constitution: the AGENTS.md mandates, their
//! declared tool grants, the acceptance command, and the `Task:` trailer
//! requirement — the brief every delegated agent receives.

use std::collections::BTreeSet;
use std::path::PathBuf;

use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};

use crate::cloud_failover::{AttemptOutcome, Runner};
use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, JobOutcome, Shared};

/// A task spec from the queue.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// Task id (`T-...`).
    pub id: String,
    /// Task-class hint used for intent-based model selection.
    pub task_class: String,
    /// Acceptance command (argv) the worker must leave passing.
    pub accept: Vec<String>,
    /// Human summary handed to the worker.
    pub title: String,
}

/// Claim denied — someone else holds it or the lease raced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimDenied {
    /// Live claim by another worker.
    Held { by: String },
    /// Task vanished or deps reopened between ready() and claim().
    NotReady,
}

/// The queue the scheduler reads. Implementations must make `claim`
/// atomic (compare-and-swap on the claim ref).
pub trait TaskQueue: Send + Sync {
    /// Open, dependency-satisfied, unclaimed tasks.
    fn ready(&self) -> Vec<TaskSpec>;
    /// Atomically claim `id` for `worker` until `lease_until` (unix secs).
    /// A denied race returns `Err` — the task is simply skipped.
    fn claim(&self, id: &str, worker: &str, lease_until: u64) -> Result<(), ClaimDenied>;
    /// Record the task finished (acceptance already verified by worker).
    fn finish(&self, id: &str);
}

/// Worktree provisioner — hermetic impls return temp dirs.
pub trait Workspaces: Send + Sync {
    /// Prepare an isolated workspace for `worker` on `task`, based on the
    /// current main. Never returns the primary checkout path.
    fn prepare(&self, task: &str, worker: &str) -> Result<PathBuf, String>;
}

/// Declared tool grants for a worker — least privilege, explicitly listed.
#[derive(Debug, Clone, Default)]
pub struct ToolGrants {
    /// Allowed tool names (empty = read-only).
    pub tools: BTreeSet<String>,
}

/// Everything a worker needs to run its task.
#[derive(Debug, Clone)]
pub struct WorkerBrief {
    /// Task spec.
    pub task: TaskSpec,
    /// The worker's own identity (`agent-task` suffix for audit).
    pub worker: String,
    /// Its private worktree.
    pub worktree: PathBuf,
    /// Opaque model id it dispatched onto (never key material).
    pub model: String,
    /// Mandates text (AGENTS.md constitution).
    pub mandates: String,
    /// Declared tool grants.
    pub grants: ToolGrants,
}

/// Execution result from the worker.
#[derive(Debug)]
pub enum ExecResult {
    /// Acceptance passed — safe to finish.
    Accepted,
    /// The attempt failed before/while running (classify like dispatch).
    Failed(String),
}

/// What actually runs the worker — production shells an agent CLI; tests
/// inject fakes.
pub trait AgentExecutor: Send + Sync {
    fn execute(&self, brief: &WorkerBrief) -> ExecResult;
}

/// The constitution handed to every worker.
#[must_use]
pub fn worker_mandates() -> String {
    [
        "You are bound by AGENTS.md identity mandates 48-56:",
        "- work ONLY in the worktree you were given; never primary, never main",
        "- every commit ends with `Task: <id>` naming your task",
        "- verify acceptance before reporting done",
        "- a live claim you do not hold is untouchable",
    ]
    .join("\n")
}

/// Per-round report.
#[derive(Debug)]
pub struct RoundReport {
    /// Tasks claimed+executed this round.
    pub ran: Vec<String>,
    /// Tasks lost to claim races.
    pub races_lost: Vec<String>,
    /// Tasks skipped: no executor/workspace capacity.
    pub skipped: Vec<String>,
    /// Per-job dispatch outcomes.
    pub outcomes: Vec<JobOutcome>,
}

/// A claimed task bound to its worker identity and isolated workspace.
#[derive(Debug)]
pub struct ClaimedWork {
    /// Task spec.
    pub task: TaskSpec,
    /// Worker identity holding the claim.
    pub worker: String,
    /// Private worktree prepared for this task.
    pub worktree: PathBuf,
}

/// Result of the claim+prepare front phase shared by `run_round` and the
/// production lifecycle.
#[derive(Debug, Default)]
pub struct ClaimBatch {
    /// Dispatchable jobs (one per claimed task).
    pub jobs: Vec<Job>,
    /// Per-task claim contexts (task, worker, worktree).
    pub claimed: Vec<ClaimedWork>,
    /// Tasks lost to claim races.
    pub races_lost: Vec<String>,
    /// Tasks skipped (not ready or no workspace).
    pub skipped: Vec<String>,
}

/// Claim every ready task and prepare its isolated workspace — the shared
/// front half of `run_round` and the production lifecycle.
pub fn claim_and_prepare<Q, W>(
    queue: &Q,
    workspaces: &W,
    agent_prefix: &str,
    lease_until: u64,
) -> ClaimBatch
where
    Q: TaskQueue + ?Sized,
    W: Workspaces + ?Sized,
{
    let mut batch = ClaimBatch::default();
    for task in queue.ready() {
        let worker = format!("{agent_prefix}-{}", task.id);
        match queue.claim(&task.id, &worker, lease_until) {
            Ok(()) => {}
            Err(ClaimDenied::Held { .. }) => {
                batch.races_lost.push(task.id.clone());
                continue;
            }
            Err(ClaimDenied::NotReady) => {
                batch.skipped.push(task.id.clone());
                continue;
            }
        }
        match workspaces.prepare(&task.id, &worker) {
            Ok(wt) => {
                batch.claimed.push(ClaimedWork {
                    task: task.clone(),
                    worker,
                    worktree: wt,
                });
                batch.jobs.push(Job {
                    id: task.id.clone(),
                    intent: IntentConstraints {
                        task_class: task.task_class.clone(),
                        discovery_budget: 3,
                        ..Default::default()
                    },
                });
            }
            Err(_) => batch.skipped.push(task.id.clone()),
        }
    }
    batch
}

/// Runner that maps a candidate attempt onto the agent executor.
pub struct AgentRunner<'a, E: AgentExecutor + ?Sized> {
    /// The worker executor (agent CLI in production, fake in tests).
    pub executor: &'a E,
    /// Brief template — `model` is overwritten per attempt.
    pub brief: WorkerBrief,
    /// Candidate inventory for opaque-id stamping.
    pub candidates: &'a [Candidate],
}

impl<'a, E: AgentExecutor + ?Sized> AgentRunner<'a, E> {
    /// Bind an executor to a worker brief and candidate pool.
    #[must_use]
    pub fn new(executor: &'a E, brief: WorkerBrief, candidates: &'a [Candidate]) -> Self {
        Self {
            executor,
            brief,
            candidates,
        }
    }
}

impl<E: AgentExecutor + ?Sized> Runner for AgentRunner<'_, E> {
    fn attempt(&mut self, index: usize, _remaining_ms: u64) -> AttemptOutcome {
        let Some(c) = self.candidates.get(index) else {
            return AttemptOutcome::PreDispatch(
                susi_vendor_models::cloud_eligibility::InferenceResult::Failed {
                    status: None,
                    body_snippet: "no such candidate".into(),
                    retry_after_secs: None,
                },
            );
        };
        let mut brief = self.brief.clone();
        brief.model = c.opaque_id();
        match self.executor.execute(&brief) {
            ExecResult::Accepted => AttemptOutcome::Success(brief.model),
            // Executor-side failure — never recorded as model evidence,
            // so a broken worker can't poison the pool for siblings.
            ExecResult::Failed(why) => AttemptOutcome::WorkerFailed(why),
        }
    }
}

/// One scheduling round: claim ready tasks, dispatch each to working
/// models via the parallel scheduler.
#[allow(clippy::too_many_arguments)] // every dependency is an injected seam
pub fn run_round<E, Q, W>(
    queue: &Q,
    workspaces: &W,
    executor: &E,
    shared: &Shared<'_>,
    plan: DispatchPlan,
    candidates: &[Candidate],
    agent_prefix: &str,
    lease_until: u64,
    grants: ToolGrants,
    mandates: &str,
) -> RoundReport
where
    E: AgentExecutor,
    Q: TaskQueue,
    W: Workspaces,
{
    let batch = claim_and_prepare(queue, workspaces, agent_prefix, lease_until);
    let jobs = batch.jobs;
    let ctxs = batch.claimed;
    let mut report = RoundReport {
        ran: Vec::new(),
        races_lost: batch.races_lost,
        skipped: batch.skipped,
        outcomes: Vec::new(),
    };
    if jobs.is_empty() {
        return report;
    }
    // Run the claimed jobs in parallel across working models. Each runner
    // executes the claimed task in its private worktree.
    let mandates_owned = mandates.to_string();
    let outcomes = run_jobs(&jobs, candidates, shared, plan, &|job: &Job| {
        let cw = ctxs
            .iter()
            .find(|c| c.task.id == job.id)
            .map(|c| ClaimedWork {
                task: c.task.clone(),
                worker: c.worker.clone(),
                worktree: c.worktree.clone(),
            })
            .unwrap_or_else(|| ClaimedWork {
                task: TaskSpec {
                    id: job.id.clone(),
                    task_class: "general".into(),
                    accept: vec![],
                    title: String::new(),
                },
                worker: format!("{agent_prefix}-{}", job.id),
                worktree: PathBuf::from("/nonexistent"),
            });
        AgentRunner::new(
            executor,
            WorkerBrief {
                task: cw.task,
                worker: cw.worker,
                worktree: cw.worktree,
                model: String::new(), // set per attempt
                mandates: mandates_owned.clone(),
                grants: grants.clone(),
            },
            candidates,
        )
    });
    for o in &outcomes {
        if o.output.is_some() {
            queue.finish(&o.job_id);
            report.ran.push(o.job_id.clone());
        }
    }
    report.outcomes = outcomes;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_vendor_models::cloud_eligibility::EligibilityStore;
    use susi_vendor_models::cloud_quota::QuotaInventory;

    /// In-memory queue: CAS claim under a mutex.
    type TaskMap = Mutex<BTreeMap<String, (TaskSpec, Option<String>)>>;

    struct FakeQueue {
        tasks: TaskMap,
        finished: Mutex<Vec<String>>,
    }
    impl FakeQueue {
        fn new(ids: &[&str]) -> Self {
            let tasks = ids
                .iter()
                .map(|id| {
                    (
                        id.to_string(),
                        (
                            TaskSpec {
                                id: id.to_string(),
                                task_class: "coding".into(),
                                accept: vec!["true".into()],
                                title: id.to_string(),
                            },
                            None,
                        ),
                    )
                })
                .collect();
            Self {
                tasks: Mutex::new(tasks),
                finished: Mutex::new(Vec::new()),
            }
        }
    }
    impl TaskQueue for FakeQueue {
        fn ready(&self) -> Vec<TaskSpec> {
            self.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|(_, claim)| claim.is_none())
                .map(|(t, _)| t.clone())
                .collect()
        }
        fn claim(&self, id: &str, worker: &str, _lease: u64) -> Result<(), ClaimDenied> {
            let mut m = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            match m.get_mut(id) {
                Some((_, slot @ None)) => {
                    *slot = Some(worker.to_string());
                    Ok(())
                }
                Some((_, Some(by))) => Err(ClaimDenied::Held { by: by.clone() }),
                None => Err(ClaimDenied::NotReady),
            }
        }
        fn finish(&self, id: &str) {
            self.finished
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(id.to_string());
        }
    }

    /// Per-task unique tempdir workspaces.
    struct FakeWorkspaces {
        dirs: Mutex<Vec<PathBuf>>,
        root: PathBuf,
    }
    impl Workspaces for FakeWorkspaces {
        fn prepare(&self, task: &str, worker: &str) -> Result<PathBuf, String> {
            let dir = self.root.join(format!("{worker}-{task}"));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            self.dirs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(dir.clone());
            Ok(dir)
        }
    }

    /// Executor recording overlap + the brief it received.
    struct RecExec {
        spans: Mutex<Vec<(Instant, Instant, String)>>,
        briefs: Mutex<Vec<WorkerBrief>>,
        sleep: Duration,
        fail_tasks: BTreeSet<String>,
    }
    impl AgentExecutor for RecExec {
        fn execute(&self, brief: &WorkerBrief) -> ExecResult {
            let start = Instant::now();
            std::thread::sleep(self.sleep);
            let end = Instant::now();
            self.spans.lock().unwrap_or_else(|e| e.into_inner()).push((
                start,
                end,
                brief.task.id.clone(),
            ));
            self.briefs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(brief.clone());
            if self.fail_tasks.contains(&brief.task.id) {
                ExecResult::Failed("agent died".into())
            } else {
                ExecResult::Accepted
            }
        }
    }

    fn cand(key: &str, model: &str, provider: &str) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: Some(provider.into()),
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 10,
            cost_per_mtok: Some(0.0),
            quality: BTreeMap::new(),
        }
    }

    fn mk_shared<'a>(
        elig: &'a Mutex<EligibilityStore>,
        quota: &'a QuotaInventory,
        lock: &'a Mutex<LockoutTracker>,
        ledger: &'a BudgetLedger,
    ) -> Shared<'a> {
        Shared::new(elig, quota, lock, ledger)
    }

    use crate::cloud_lockout::LockoutTracker;

    fn plan() -> DispatchPlan {
        DispatchPlan {
            max_workers: 8,
            mission: "roadmap".into(),
            job_cpu_millis: 0,
            job_ram_mb: 0,
            job_vram_mb: 0,
            job_subprocesses: 0,
            per_job: crate::cloud_failover::FailoverBudget {
                max_attempts: 4,
                deadline_ms: None,
                spend: SpendPolicy::FreeOnly,
                attempt_estimate_micros: 0,
                now_ms: 1_700_000_000_000,
            },
        }
    }

    #[test]
    fn parallel_roadmap_agents_execute_in_parallel_on_distinct_models() {
        // 3 tasks × 3 working models × 3 tempdir worktrees — real overlap.
        let queue = FakeQueue::new(&["T-1", "T-2", "T-3"]);
        let root = std::env::temp_dir().join(format!("ra-{}", std::process::id()));
        let ws = FakeWorkspaces {
            dirs: Mutex::new(Vec::new()),
            root: root.clone(),
        };
        let exec = RecExec {
            spans: Mutex::new(Vec::new()),
            briefs: Mutex::new(Vec::new()),
            sleep: Duration::from_millis(60),
            fail_tasks: BTreeSet::new(),
        };
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = mk_shared(&elig, &quota, &lock, &ledger);
        let cs = vec![
            cand("k1", "m1", "p1"),
            cand("k2", "m2", "p2"),
            cand("k3", "m3", "p3"),
        ];
        let started = Instant::now();
        let rep = run_round(
            &queue,
            &ws,
            &exec,
            &shared,
            plan(),
            &cs,
            "devin",
            u64::MAX,
            ToolGrants::default(),
            &worker_mandates(),
        );
        assert_eq!(rep.ran.len(), 3);
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "serial? {:?}",
            started.elapsed()
        );
        // Distinct worktrees per task.
        let dirs = ws.dirs.lock().unwrap_or_else(|e| e.into_inner());
        let uniq: BTreeSet<_> = dirs.iter().collect();
        assert_eq!(uniq.len(), 3);
        // Mandates were handed to every worker.
        for b in exec.briefs.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            assert!(b.mandates.contains("AGENTS.md"));
            assert!(b.worktree.starts_with(&root));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_roadmap_agents_claim_race_loses_politely() {
        // Two schedulers race the same task; exactly one runs it.
        let queue = FakeQueue::new(&["T-9"]);
        let root = std::env::temp_dir().join(format!("rb-{}", std::process::id()));
        let ws = FakeWorkspaces {
            dirs: Mutex::new(Vec::new()),
            root: root.clone(),
        };
        let exec = RecExec {
            spans: Mutex::new(Vec::new()),
            briefs: Mutex::new(Vec::new()),
            sleep: Duration::from_millis(5),
            fail_tasks: BTreeSet::new(),
        };
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = mk_shared(&elig, &quota, &lock, &ledger);
        let cs = vec![cand("k1", "m1", "p1")];
        let r1 = run_round(
            &queue,
            &ws,
            &exec,
            &shared,
            plan(),
            &cs,
            "a",
            u64::MAX,
            ToolGrants::default(),
            "",
        );
        let r2 = run_round(
            &queue,
            &ws,
            &exec,
            &shared,
            plan(),
            &cs,
            "b",
            u64::MAX,
            ToolGrants::default(),
            "",
        );
        assert_eq!(r1.ran, vec!["T-9".to_string()]);
        assert!(r2.ran.is_empty());
        assert_eq!(queue.ready().len(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_roadmap_agents_workspaces_are_isolated() {
        let queue = FakeQueue::new(&["T-1", "T-2"]);
        let root = std::env::temp_dir().join(format!("rc-{}", std::process::id()));
        let ws = FakeWorkspaces {
            dirs: Mutex::new(Vec::new()),
            root: root.clone(),
        };
        let exec = RecExec {
            spans: Mutex::new(Vec::new()),
            briefs: Mutex::new(Vec::new()),
            sleep: Duration::from_millis(1),
            fail_tasks: BTreeSet::new(),
        };
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = mk_shared(&elig, &quota, &lock, &ledger);
        let cs = vec![cand("k1", "m1", "p1"), cand("k2", "m2", "p2")];
        let rep = run_round(
            &queue,
            &ws,
            &exec,
            &shared,
            plan(),
            &cs,
            "d",
            u64::MAX,
            ToolGrants::default(),
            "",
        );
        assert_eq!(rep.ran.len(), 2);
        let dirs = ws.dirs.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(dirs.len(), 2);
        assert_ne!(dirs[0], dirs[1]);
        // Workspaces live under the scratch root — never the primary checkout.
        for d in dirs.iter() {
            assert!(d.starts_with(&root));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_roadmap_agents_failed_task_is_not_finished() {
        let queue = FakeQueue::new(&["T-ok", "T-bad"]);
        let root = std::env::temp_dir().join(format!("rd-{}", std::process::id()));
        let ws = FakeWorkspaces {
            dirs: Mutex::new(Vec::new()),
            root: root.clone(),
        };
        let mut fail = BTreeSet::new();
        fail.insert("T-bad".to_string());
        let exec = RecExec {
            spans: Mutex::new(Vec::new()),
            briefs: Mutex::new(Vec::new()),
            sleep: Duration::from_millis(1),
            fail_tasks: fail,
        };
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = mk_shared(&elig, &quota, &lock, &ledger);
        let cs = vec![cand("k1", "m1", "p1"), cand("k2", "m2", "p2")];
        let rep = run_round(
            &queue,
            &ws,
            &exec,
            &shared,
            plan(),
            &cs,
            "d",
            u64::MAX,
            ToolGrants::default(),
            "",
        );
        // T-bad fails on both candidates (same runner), T-ok finishes.
        assert_eq!(rep.ran, vec!["T-ok".to_string()]);
        let fin = queue.finished.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(fin.as_slice(), &["T-ok".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
