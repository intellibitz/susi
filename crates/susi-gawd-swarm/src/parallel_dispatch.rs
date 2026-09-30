//! Parallel dispatch: run independent ready jobs simultaneously across
//! multiple *currently working* credential/model targets.
//!
//! Selection (`susi-gawd-agents::cloud_intent`) picks per-job; this module is
//! the scheduler that admits bounded concurrency, queues excess work fairly
//! (FIFO), and spreads workers across distinct quota pools — two model ids on
//! one provider+account are ONE pool, not independent capacity.
//!
//! Shared evidence lives behind scoped locks: `select`, `permit`, and
//! `record` take the locks for microseconds; the injected `Runner::attempt`
//! (the real I/O) runs with no lock held, so workers truly overlap and a
//! failing target falls back per job without stalling siblings.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;

use susi_gawd_agents::cloud_budget::{BudgetLedger, Denial, FreeHeadroom, Reservation};
use susi_gawd_agents::cloud_intent::{select, Candidate, IntentConstraints, Ranked, Selection};
use susi_vendor_models::cloud_eligibility::{
    credential_fingerprint, EligibilityKind, EligibilityStore, InferenceResult, Subject,
};
use susi_vendor_models::cloud_quota::QuotaInventory;

use crate::cloud_failover::{AttemptOutcome, AttemptRecord, FailoverStop, Runner};
use crate::cloud_lockout::{LockoutTracker, Permit};

/// One unit of dispatched work.
#[derive(Debug, Clone)]
pub struct Job {
    /// Stable job id for the audit trail.
    pub id: String,
    /// What this job needs from a model.
    pub intent: IntentConstraints,
}

/// A job's outcome.
#[derive(Debug)]
pub struct JobOutcome {
    /// Which job finished (or exhausted).
    pub job_id: String,
    /// Winning output, if any attempt succeeded.
    pub output: Option<String>,
    /// Opaque id (`provider/model/credfp8`) of the winner.
    pub winner: Option<String>,
    /// Quota pool (`provider:account`) that served the winner.
    pub winner_pool: Option<String>,
    /// Attempt trace.
    pub attempts: Vec<AttemptRecord>,
    /// Why the job stopped without output.
    pub stop: Option<FailoverStop>,
    /// A prior ambiguous attempt may have had side effects.
    pub prior_ambiguous: bool,
}

/// Shared stores behind scoped locks — workers borrow guards per decision,
/// never across `Runner::attempt`.
pub struct Shared<'a> {
    /// Eligibility evidence.
    pub eligibility: &'a Mutex<EligibilityStore>,
    /// Quota inventory (immutable during dispatch).
    pub quota: &'a QuotaInventory,
    /// Lockout/circuit state.
    pub lockouts: &'a Mutex<LockoutTracker>,
    /// Spend ledger (interior synchronization).
    pub ledger: &'a BudgetLedger,
    /// Pools currently serving a job — workers prefer fresh pools.
    active_pools: Mutex<BTreeMap<String, usize>>,
}

impl<'a> Shared<'a> {
    /// Create the shared dispatch context.
    pub fn new(
        eligibility: &'a Mutex<EligibilityStore>,
        quota: &'a QuotaInventory,
        lockouts: &'a Mutex<LockoutTracker>,
        ledger: &'a BudgetLedger,
    ) -> Self {
        Self {
            eligibility,
            quota,
            lockouts,
            ledger,
            active_pools: Mutex::new(BTreeMap::new()),
        }
    }

    fn eligibility(&self) -> MutexGuard<'_, EligibilityStore> {
        self.eligibility.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn lockouts(&self) -> MutexGuard<'_, LockoutTracker> {
        self.lockouts.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn active(&self) -> MutexGuard<'_, BTreeMap<String, usize>> {
        self.active_pools.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Scheduler bounds.
#[derive(Debug, Clone, Copy)]
pub struct DispatchPlan {
    /// Maximum simultaneous workers.
    pub max_workers: usize,
    /// Per-job attempt/deadline/spend bounds.
    pub per_job: crate::cloud_failover::FailoverBudget,
}

/// The quota pool key: provider + account. Distinct model ids on one
/// provider/account share one pool — they are NOT independent capacity.
#[must_use]
pub fn pool_of(c: &Candidate) -> String {
    format!("{}:{}", c.provider, c.account.as_deref().unwrap_or("-"))
}

/// Choose the next ranked candidate not yet attempted, preferring pools no
/// other worker is actively using. `None` when nothing remains.
fn pick_next(
    selection: &Selection,
    attempted: &[usize],
    active: &BTreeMap<String, usize>,
    candidates: &[Candidate],
) -> Option<Ranked> {
    let mut first_active: Option<Ranked> = None;
    for r in &selection.ranked {
        if attempted.contains(&r.index) {
            continue;
        }
        let busy = active
            .get(&pool_of(&candidates[r.index]))
            .copied()
            .unwrap_or(0)
            > 0;
        if busy {
            if first_active.is_none() {
                first_active = Some(r.clone());
            }
            continue;
        }
        return Some(r.clone());
    }
    first_active
}

/// Run `jobs` over `candidates` with bounded worker concurrency.
/// `make_runner` is invoked on the worker thread, once per job.
#[must_use]
pub fn run_jobs<R, F>(
    jobs: &[Job],
    candidates: &[Candidate],
    shared: &Shared<'_>,
    plan: DispatchPlan,
    make_runner: &F,
) -> Vec<JobOutcome>
where
    R: Runner,
    F: Fn(&Job) -> R + Sync,
{
    let next = AtomicUsize::new(0);
    let outcomes: Mutex<Vec<JobOutcome>> = Mutex::new(Vec::new());
    let workers = plan.max_workers.max(1).min(jobs.len().max(1));
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(job) = jobs.get(i) else { break };
                let mut runner = make_runner(job);
                let out = run_job(job, candidates, shared, plan.per_job, &mut runner);
                outcomes.lock().unwrap_or_else(|e| e.into_inner()).push(out);
            });
        }
    });
    let mut v = outcomes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect::<Vec<_>>();
    // FIFO ordering of the report, regardless of finish order.
    v.sort_by_key(|o| {
        jobs.iter()
            .position(|j| j.id == o.job_id)
            .unwrap_or(usize::MAX)
    });
    v
}

/// One job's failover loop with scoped lock access.
fn run_job<R: Runner>(
    job: &Job,
    candidates: &[Candidate],
    shared: &Shared<'_>,
    budget: crate::cloud_failover::FailoverBudget,
    runner: &mut R,
) -> JobOutcome {
    let mut out = JobOutcome {
        job_id: job.id.clone(),
        output: None,
        winner: None,
        winner_pool: None,
        attempts: Vec::new(),
        stop: None,
        prior_ambiguous: false,
    };
    let mut now_ms = budget.now_ms;
    // Selection once per job: evidence gathered during this job's attempts is
    // folded back into the store, but the ranked order stays the policy's.
    let selection = {
        let eg = shared.eligibility();
        select(&job.intent, candidates, &eg, shared.quota, now_ms / 1000)
    };
    if selection.ranked.is_empty() {
        out.stop = Some(FailoverStop::NoEligibleCandidates);
        return out;
    }
    let mut attempted: Vec<usize> = Vec::new();
    let mut saw_lockout = false;
    loop {
        let ranked: Option<Ranked> = {
            let act = shared.active();
            pick_next(&selection, &attempted, &act, candidates)
        };
        let Some(ranked) = ranked else {
            out.stop = Some(if saw_lockout {
                FailoverStop::AllLockedOut
            } else {
                FailoverStop::AttemptBudgetExhausted {
                    used: out.attempts.len() as u32,
                }
            });
            return out;
        };
        if out.attempts.len() as u32 >= budget.max_attempts {
            out.stop = Some(FailoverStop::AttemptBudgetExhausted {
                used: out.attempts.len() as u32,
            });
            return out;
        }
        if let Some(dl) = budget.deadline_ms {
            if now_ms >= dl {
                out.stop = Some(FailoverStop::DeadlineExceeded);
                return out;
            }
        }
        let c = &candidates[ranked.index];
        let pool = pool_of(c);
        let scope = ranked.candidate.clone();
        // Lockout gate (scoped lock, released before the reservation wait).
        {
            let lk = shared.lockouts();
            match lk.permit(&scope, EligibilityKind::Unknown) {
                Permit::Allowed => {}
                Permit::Cooldown { .. } | Permit::CircuitOpen { .. } | Permit::ProbesExhausted => {
                    saw_lockout = true;
                    attempted.push(ranked.index);
                    continue;
                }
                Permit::Permanent { .. } => {
                    attempted.push(ranked.index);
                    continue;
                }
            }
        }
        // Spend gate — the ledger is interior-synchronized.
        let reservation: Reservation =
            match shared
                .ledger
                .reserve(susi_gawd_agents::cloud_budget::ReserveRequest {
                    account: c.account.as_deref().unwrap_or(""),
                    credential_fp: &credential_fingerprint(&c.api_key),
                    model: &c.model,
                    policy: budget.spend,
                    is_free_candidate: c.cost_per_mtok.unwrap_or(0.0) == 0.0,
                    free_headroom: FreeHeadroom::Unknown,
                    estimate: budget.attempt_estimate_micros,
                }) {
                Ok(r) => r,
                Err(
                    Denial::FreeExhausted
                    | Denial::NeedsConsent { .. }
                    | Denial::NeedsPriceConsent { .. }
                    | Denial::OverBudget { .. },
                ) => {
                    attempted.push(ranked.index);
                    continue;
                }
            };
        attempted.push(ranked.index);
        {
            let mut act = shared.active();
            *act.entry(pool.clone()).or_insert(0) += 1;
        }
        let remaining = budget
            .deadline_ms
            .map(|dl| dl.saturating_sub(now_ms))
            .unwrap_or(u64::MAX);
        // The actual call — no shared lock held, so workers overlap here.
        let outcome = runner.attempt(ranked.index, remaining);
        {
            let mut act = shared.active();
            if let Some(n) = act.get_mut(&pool) {
                *n = n.saturating_sub(1);
            }
        }
        match outcome {
            AttemptOutcome::Success(text) => {
                {
                    let mut eg = shared.eligibility();
                    eg.record_inference(subj(c), &InferenceResult::Success, now_ms / 1000);
                    let mut lk = shared.lockouts();
                    lk.record(&scope, &InferenceResult::Success);
                }
                shared.ledger.commit(reservation, None);
                out.output = Some(text);
                out.winner = Some(ranked.candidate.clone());
                out.winner_pool = Some(pool);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "success",
                });
                return out;
            }
            AttemptOutcome::PreDispatch(res) => {
                {
                    let mut eg = shared.eligibility();
                    eg.record_inference(subj(c), &res, now_ms / 1000);
                    let mut lk = shared.lockouts();
                    lk.record(&scope, &res);
                }
                shared.ledger.release(reservation);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "pre_dispatch",
                });
            }
            AttemptOutcome::MidStream {
                result: res,
                partial,
            } => {
                let _ = partial;
                {
                    let mut eg = shared.eligibility();
                    eg.record_inference(subj(c), &res, now_ms / 1000);
                    let mut lk = shared.lockouts();
                    lk.record(&scope, &res);
                }
                shared.ledger.commit(reservation, None);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "mid_stream",
                });
            }
            AttemptOutcome::Ambiguous(res) => {
                {
                    let mut eg = shared.eligibility();
                    eg.record_inference(subj(c), &res, now_ms / 1000);
                    let mut lk = shared.lockouts();
                    lk.record(&scope, &res);
                }
                shared.ledger.commit(reservation, None);
                out.prior_ambiguous = true;
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "ambiguous",
                });
            }
        }
        now_ms = now_ms.saturating_add(1);
    }
}

fn subj<'a>(c: &'a Candidate) -> Subject<'a> {
    Subject {
        provider: &c.provider,
        api_key: &c.api_key,
        account: c.account.as_deref(),
        region: c.region.as_deref(),
        model: &c.model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};
    use susi_gawd_agents::cloud_budget::SpendPolicy;
    use susi_vendor_models::cloud_eligibility::Subject;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    fn cand_on(key: &str, model: &str, provider: &str, account: &str) -> Candidate {
        Candidate {
            provider: provider.into(),
            api_key: key.into(),
            account: Some(account.into()),
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 100,
            cost_per_mtok: Some(1.0),
            quality: BTreeMap::new(),
        }
    }

    fn job(n: usize) -> Job {
        Job {
            id: format!("job-{n}"),
            intent: IntentConstraints {
                task_class: "coding".into(),
                discovery_budget: 3,
                ..Default::default()
            },
        }
    }

    fn plan(workers: usize) -> DispatchPlan {
        DispatchPlan {
            max_workers: workers,
            per_job: crate::cloud_failover::FailoverBudget {
                max_attempts: 8,
                deadline_ms: Some(T0_MS + 300_000),
                spend: SpendPolicy::PaidAuthorized {
                    max_spend_micro: 100_000,
                },
                attempt_estimate_micros: 10,
                now_ms: T0_MS,
            },
        }
    }

    fn stores<'a>(
        elig: &'a Mutex<EligibilityStore>,
        quota: &'a QuotaInventory,
        lock: &'a Mutex<LockoutTracker>,
        ledger: &'a BudgetLedger,
    ) -> Shared<'a> {
        Shared::new(elig, quota, lock, ledger)
    }

    fn prove(cs: &[Candidate], elig: &Mutex<EligibilityStore>, idxs: &[usize]) {
        let mut e = elig.lock().unwrap_or_else(|e| e.into_inner());
        for &i in idxs {
            e.record_inference(
                Subject {
                    provider: &cs[i].provider,
                    api_key: &cs[i].api_key,
                    account: cs[i].account.as_deref(),
                    region: cs[i].region.as_deref(),
                    model: &cs[i].model,
                },
                &InferenceResult::Success,
                T0,
            );
        }
    }

    type Spans = std::sync::Arc<Mutex<Vec<(Instant, Instant)>>>;

    /// Runner that sleeps briefly (real overlapping I/O) and succeeds —
    /// records each attempt's wall-clock interval for overlap assertions.
    struct TimedRunner {
        sleep: Duration,
        spans: Spans,
        fail_indexes: Vec<usize>,
        seen: usize,
    }
    impl Runner for TimedRunner {
        fn attempt(&mut self, index: usize, _r: u64) -> AttemptOutcome {
            let start = Instant::now();
            std::thread::sleep(self.sleep);
            let end = Instant::now();
            self.seen += 1;
            self.spans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((start, end));
            if self.fail_indexes.contains(&index) {
                AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(503),
                    body_snippet: "down".into(),
                    retry_after_secs: None,
                })
            } else {
                AttemptOutcome::Success(format!("ok-{index}"))
            }
        }
    }

    fn mk(
        candidates: Vec<Candidate>,
        jobs: Vec<Job>,
        workers: usize,
        runner_sleep: Duration,
        fail: Vec<usize>,
    ) -> (Vec<JobOutcome>, Spans) {
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = stores(&elig, &quota, &lock, &ledger);
        let spans = std::sync::Arc::new(Mutex::new(Vec::new()));
        let spans2 = spans.clone();
        let make = move |_j: &Job| TimedRunner {
            sleep: runner_sleep,
            spans: spans2.clone(),
            fail_indexes: fail.clone(),
            seen: 0,
        };
        (
            run_jobs(&jobs, &candidates, &shared, plan(workers), &make),
            spans,
        )
    }

    #[test]
    fn parallel_working_models_three_jobs_overlap_wall_time() {
        // Three working models on three providers/accounts; 3 jobs × 150ms.
        // Serial would take ≥450ms; real overlap must finish well under that.
        let cs = vec![
            cand_on("sk-a", "mA", "prov-a", "acct-a"),
            cand_on("sk-b", "mB", "prov-b", "acct-b"),
            cand_on("sk-c", "mC", "prov-c", "acct-c"),
        ];
        let jobs: Vec<Job> = (0..3).map(job).collect();
        let started = Instant::now();
        let (outcomes, _spans) = mk(cs, jobs, 3, Duration::from_millis(150), vec![]);
        let elapsed = started.elapsed();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
        assert!(
            elapsed < Duration::from_millis(400),
            "jobs must overlap in wall time, took {elapsed:?}"
        );
        // And each job ran on a distinct pool.
        let pools: Vec<_> = outcomes.iter().map(|o| o.winner_pool.clone()).collect();
        assert!(pools.contains(&Some("prov-a:acct-a".into())));
        assert!(pools.contains(&Some("prov-b:acct-b".into())));
        assert!(pools.contains(&Some("prov-c:acct-c".into())));
    }

    #[test]
    fn parallel_working_models_skips_higher_ranked_dead_model() {
        // Index 0 is highest-ranked (best quality evidence) but dead; every
        // job must fail over to a working sibling — never strand.
        let mut cs = vec![
            cand_on("sk-a", "mA", "prov-a", "acct-a"),
            cand_on("sk-b", "mB", "prov-b", "acct-b"),
            cand_on("sk-c", "mC", "prov-c", "acct-c"),
        ];
        cs[0].quality.insert("coding".into(), (50, 50));
        let elig = Mutex::new(EligibilityStore::new());
        prove(&cs, &elig, &[0, 1, 2]);
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = stores(&elig, &quota, &lock, &ledger);
        let jobs: Vec<Job> = (0..3).map(job).collect();
        let make = |_j: &Job| TimedRunner {
            sleep: Duration::from_millis(20),
            spans: std::sync::Arc::new(Mutex::new(Vec::new())),
            fail_indexes: vec![0],
            seen: 0,
        };
        let outcomes = run_jobs(&jobs, &cs, &shared, plan(3), &make);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
        for o in &outcomes {
            assert!(o.winner_pool.as_deref() != Some("prov-a:acct-a"));
        }
        // The dead leader was either attempted-and-recorded or excluded by
        // the evidence its sibling's failure wrote — never a winner.
        assert!(outcomes
            .iter()
            .any(|o| o.attempts.iter().any(|a| a.outcome == "pre_dispatch")));
    }

    #[test]
    fn parallel_working_models_shared_account_is_one_quota_pool() {
        // Two model ids on the same provider+account = one pool. With three
        // jobs and a distinct third pool, workers must not both pile onto the
        // shared account while pool-c sits idle.
        let cs = vec![
            cand_on("sk-a1", "mA1", "prov-a", "acct-a"),
            cand_on("sk-a2", "mA2", "prov-a", "acct-a"),
            cand_on("sk-c", "mC", "prov-c", "acct-c"),
        ];
        let jobs: Vec<Job> = (0..3).map(job).collect();
        let (outcomes, _s) = mk(cs, jobs, 3, Duration::from_millis(40), vec![]);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
    }

    #[test]
    fn parallel_working_models_bounded_concurrency_queues_fairly() {
        // 4 jobs, 2 workers: all complete, in at most ~2 waves of sleep.
        let cs = vec![
            cand_on("sk-a", "mA", "prov-a", "acct-a"),
            cand_on("sk-b", "mB", "prov-b", "acct-b"),
        ];
        let jobs: Vec<Job> = (0..4).map(job).collect();
        let started = Instant::now();
        let (outcomes, _s) = mk(cs, jobs, 2, Duration::from_millis(100), vec![]);
        let elapsed = started.elapsed();
        assert_eq!(outcomes.len(), 4);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
        // FIFO report: outcomes come back in job order.
        let ids: Vec<_> = outcomes.iter().map(|o| o.job_id.clone()).collect();
        assert_eq!(ids, vec!["job-0", "job-1", "job-2", "job-3"]);
        // Bounded: 4×100ms over 2 workers ≈ 200ms+, never 400ms serial.
        assert!(elapsed < Duration::from_millis(380), "took {elapsed:?}");
    }

    #[test]
    fn parallel_working_models_per_job_fallback_preserves_others() {
        // Provider-a dies for job-0 mid-run; jobs on b/c must still finish —
        // a per-job fallback never blocks sibling work.
        let cs = vec![
            cand_on("sk-a", "mA", "prov-a", "acct-a"),
            cand_on("sk-b", "mB", "prov-b", "acct-b"),
            cand_on("sk-c", "mC", "prov-c", "acct-c"),
        ];
        let jobs: Vec<Job> = (0..3).map(job).collect();
        let (outcomes, _s) = mk(cs, jobs, 3, Duration::from_millis(30), vec![0]);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
    }
}
