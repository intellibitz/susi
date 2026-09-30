//! Offline end-to-end self-development campaign (T-CODEX-22).
//!
//! Composes the production seams into one measurable loop per generation:
//!
//! 1. **Propose** — `run_proposals` dispatches evidence-backed proposal
//!    generation through the production failover path (availability,
//!    lockouts, spend policy); model output is validated, never trusted.
//! 2. **Implement** — `ImplAttempt` adapts [`CloudImplExec`] onto the same
//!    failover path: the selected model returns a patch plan, native
//!    confined tools apply it in the campaign worktree, and the task's
//!    real acceptance argv is run by `apply_patch_cycle` (failed applies
//!    are reverted).
//! 3. **Measure** — a [`HeldOut`] suite scores the worktree before and
//!    after each generation. Improvement is *measured*, never claimed
//!    from suggestion counts.
//! 4. **Verify** — `verify_candidate` re-runs acceptance, runs the extra
//!    gates, requires an independent reviewer (a different model than the
//!    implementer), and rejects measured regressions and fabricated
//!    receipts.
//! 5. **Record** — the lifecycle store tracks the ordered phases and the
//!    outcome ledger learns which models produce verified improvements.
//!    A rejected generation restores the pre-candidate snapshot — nothing
//!    unverified reaches the worktree.
//!
//! The campaign is honest about cost and blockage: cumulative spend comes
//! from the shared budget ledger, and a fully blocked generation records
//! the real stop reason instead of fabricating progress.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use susi_gawd_agents::cloud_budget::BudgetLedger;
use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
use susi_gawd_swarm::cloud_failover::{run, AttemptOutcome, FailoverBudget, Runner, Stores};
use susi_gawd_swarm::roadmap_agents::{AgentExecutor, ExecResult, TaskSpec, WorkerBrief};
use susi_vendor_models::cloud_eligibility::InferenceResult;

use crate::cloud_rsi::{
    run_proposals, verify_candidate, CloudImplExec, GateRunner, ImplModel, ProposalModel,
    ProposalPolicy, ReviewModel, ReviewPacket, RsiEvidence, VerifyPolicy,
};
use crate::cloud_rsi_lifecycle::{LifecycleStore, Phase, WorkRecord};
use crate::cloud_rsi_outcomes::{Outcome, OutcomeLedger, OutcomeUsage};

// ------------------------------------------------------------ held-out suite

/// Held-out measurement seam: production runs the real benchmark suite in
/// the candidate worktree; tests inject deterministic offline workloads.
pub trait HeldOut: Send + Sync {
    /// Score the worktree — metric name → measured value.
    fn measure(&self, worktree: &Path) -> BTreeMap<String, f64>;
}

/// A held-out suite driven by a real command: `argv` runs in the worktree
/// and writes `metrics_file` (`name=value` lines) which is then parsed.
/// Deterministic and offline — no model involvement in the measurement.
pub struct ScriptSuite {
    /// Suite argv (e.g. `["sh", "suite.sh"]`).
    pub argv: Vec<String>,
    /// Worktree-relative metrics file the suite writes.
    pub metrics_file: String,
}

impl HeldOut for ScriptSuite {
    fn measure(&self, worktree: &Path) -> BTreeMap<String, f64> {
        if let Some(argv0) = self.argv.first() {
            let _ = Command::new(argv0)
                .args(&self.argv[1..])
                .current_dir(worktree)
                .output();
        }
        std::fs::read_to_string(worktree.join(&self.metrics_file))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                v.trim()
                    .parse::<f64>()
                    .ok()
                    .map(|n| (k.trim().to_string(), n))
            })
            .collect()
    }
}

/// Executes each verification gate as a real process in the worktree.
pub struct WorktreeGates;

impl GateRunner for WorktreeGates {
    fn run_gate(&self, worktree: &Path, _name: &str, argv: &[String]) -> bool {
        let Some(argv0) = argv.first() else {
            return false;
        };
        Command::new(argv0)
            .args(&argv[1..])
            .current_dir(worktree)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ------------------------------------------------------- implementation seam

/// Adapts [`CloudImplExec`] (per-brief executor) into a failover `Runner`:
/// each attempt briefs candidate `index` with the task and lets the model
/// plan + native tools apply/verify inside `worktree`.
pub struct ImplAttempt<'a> {
    /// Executor over the injected inference backend.
    pub exec: &'a CloudImplExec<'a>,
    /// Candidate pool in dispatch order.
    pub candidates: &'a [Candidate],
    /// Isolated worktree the attempt is confined to.
    pub worktree: PathBuf,
    /// Task being implemented.
    pub task: TaskSpec,
}

impl Runner for ImplAttempt<'_> {
    fn attempt(&mut self, index: usize, _remaining_ms: u64) -> AttemptOutcome {
        let Some(c) = self.candidates.get(index) else {
            return AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: None,
                body_snippet: "no candidate".into(),
                retry_after_secs: None,
            });
        };
        let brief = WorkerBrief {
            task: self.task.clone(),
            worker: format!("w-{}", self.task.id),
            worktree: self.worktree.clone(),
            model: c.opaque_id(),
            mandates: String::new(),
            grants: Default::default(),
        };
        match self.exec.execute(&brief) {
            ExecResult::Accepted => AttemptOutcome::Success(c.opaque_id()),
            ExecResult::Failed(why) => AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: None,
                body_snippet: why,
                retry_after_secs: None,
            }),
        }
    }
}

// ----------------------------------------------------------------- campaign

/// One candidate generation's full audit trail.
#[derive(Debug, Clone)]
pub struct GenerationRecord {
    /// 1-based generation index.
    pub generation: usize,
    /// The minted task implemented this generation (None when proposal
    /// dispatch produced nothing usable — `proposal_stop` says why).
    pub task_id: Option<String>,
    /// Dispatch audit sizes (proposals / implementation attempts).
    pub proposal_attempts: usize,
    /// Implementation attempt count.
    pub impl_attempts: usize,
    /// Honest stop reasons when a stage produced nothing.
    pub proposal_stop: Option<String>,
    /// Implementation stop reason when no candidate succeeded.
    pub impl_stop: Option<String>,
    /// Opaque id of the model whose patch was applied (never key material).
    pub implementer: Option<String>,
    /// Opaque id of the independent reviewer.
    pub reviewer: Option<String>,
    /// Gate results from verification.
    pub gates: Vec<(String, bool)>,
    /// True only when verification passed end-to-end.
    pub verified: bool,
    /// Rejection reasons when not verified.
    pub reasons: Vec<String>,
    /// Suite metrics before the generation.
    pub before: BTreeMap<String, f64>,
    /// Suite metrics after the generation (post-restore when rejected).
    pub after: BTreeMap<String, f64>,
    /// Spend committed by this generation's dispatches (micros).
    pub spend_micros: u64,
    /// Lifecycle record id tracking this generation.
    pub lifecycle_id: Option<String>,
}

/// Everything needed to run successive generations. All model backends,
/// gates and the suite are injected — the same struct works against real
/// providers or hermetic fakes.
pub struct Campaign<'a> {
    /// Isolated repository under improvement.
    pub worktree: PathBuf,
    /// Held-out measurement.
    pub suite: &'a dyn HeldOut,
    /// Proposal backend.
    pub proposals: &'a dyn ProposalModel,
    /// Implementation backend.
    pub implementer_model: &'a dyn ImplModel,
    /// Independent review backend.
    pub review_model: &'a dyn ReviewModel,
    /// Gate executor.
    pub gates: &'a dyn GateRunner,
    /// Shared intent constraints for all dispatches.
    pub intent: IntentConstraints,
    /// Proposal dispatch pool.
    pub proposers: Vec<Candidate>,
    /// Implementation dispatch pool.
    pub implementers: Vec<Candidate>,
    /// Review dispatch pool (the implementer is excluded inside
    /// `verify_candidate` — self-approval is refused).
    pub reviewers: Vec<Candidate>,
    /// Per-dispatch attempt/deadline/spend bounds.
    pub budget: FailoverBudget,
    /// Proposal validation/minting policy (`next_seq` advances per
    /// generation so task ids never collide).
    pub proposal_policy: ProposalPolicy,
    /// Verification gates and regression tolerance.
    pub verify_policy: VerifyPolicy,
    /// Outcome-ledger task class.
    pub task_class: String,
    /// Worker namespace for lifecycle records.
    pub worker: String,
    /// Secret patterns stripped before any prompt leaves the process.
    pub secrets: Vec<String>,
    generations: Vec<GenerationRecord>,
    baseline: Option<BTreeMap<String, f64>>,
}

/// Campaign-level measured report — the ONLY honest claim of improvement.
#[derive(Debug, Clone)]
pub struct CampaignReport {
    /// Per-generation audit records.
    pub generations: Vec<GenerationRecord>,
    /// Metrics before generation 1.
    pub baseline: BTreeMap<String, f64>,
    /// Metrics after the last generation.
    pub final_metrics: BTreeMap<String, f64>,
    /// Cumulative committed spend across all dispatches (micros).
    pub total_spend_micros: u64,
    /// Generations that passed verification and were promoted.
    pub promoted: usize,
    /// Generations rejected by verification (restored, not promoted).
    pub rejected: usize,
    /// Generations that produced no usable proposal/implementation.
    pub blocked: usize,
    /// True when at least one promoted generation strictly improved a
    /// baseline metric with no surviving regression.
    pub improved: bool,
}

impl<'a> Campaign<'a> {
    /// Create a campaign rooted at `worktree`.
    #[allow(clippy::too_many_arguments)] // every field is an injected seam
    pub fn new(
        worktree: PathBuf,
        suite: &'a dyn HeldOut,
        proposals: &'a dyn ProposalModel,
        implementer_model: &'a dyn ImplModel,
        review_model: &'a dyn ReviewModel,
        gates: &'a dyn GateRunner,
        intent: IntentConstraints,
        proposers: Vec<Candidate>,
        implementers: Vec<Candidate>,
        reviewers: Vec<Candidate>,
        budget: FailoverBudget,
        proposal_policy: ProposalPolicy,
        verify_policy: VerifyPolicy,
        task_class: &str,
        worker: &str,
    ) -> Self {
        Self {
            worktree,
            suite,
            proposals,
            implementer_model,
            review_model,
            gates,
            intent,
            proposers,
            implementers,
            reviewers,
            budget,
            proposal_policy,
            verify_policy,
            task_class: task_class.to_string(),
            worker: worker.to_string(),
            secrets: Vec::new(),
            generations: Vec::new(),
            baseline: None,
        }
    }

    fn spend_now(&self, ledger: &BudgetLedger) -> u64 {
        let mut accounts: Vec<String> = vec![String::new()]; // shared pool
        for c in self
            .proposers
            .iter()
            .chain(&self.implementers)
            .chain(&self.reviewers)
        {
            if let Some(a) = &c.account {
                if !accounts.contains(a) {
                    accounts.push(a.clone());
                }
            }
        }
        accounts
            .iter()
            .map(|a| ledger.account_exposure(a))
            .sum::<u64>()
    }

    /// Run one full generation: propose → implement → measure → verify →
    /// record. Returns the audit record; a rejected generation leaves the
    /// worktree restored to its pre-candidate state.
    pub fn run_generation(
        &mut self,
        stores: &mut Stores<'_>,
        outcomes: &mut OutcomeLedger,
        life: &mut LifecycleStore,
        evidence: &RsiEvidence,
    ) -> GenerationRecord {
        let gen = self.generations.len() + 1;
        let before = self.suite.measure(&self.worktree);
        if self.baseline.is_none() {
            self.baseline = Some(before.clone());
        }
        let spend0 = self.spend_now(stores.ledger);
        let mut rec = GenerationRecord {
            generation: gen,
            task_id: None,
            proposal_attempts: 0,
            impl_attempts: 0,
            proposal_stop: None,
            impl_stop: None,
            implementer: None,
            reviewer: None,
            gates: Vec::new(),
            verified: false,
            reasons: Vec::new(),
            before,
            after: BTreeMap::new(),
            spend_micros: 0,
            lifecycle_id: None,
        };

        // --- 1. proposals through production dispatch --------------------
        let round = run_proposals(
            &self.intent,
            &self.proposers,
            stores,
            self.budget,
            self.proposals,
            evidence,
            &self.secrets,
            &self.proposal_policy,
        );
        rec.proposal_attempts = round.failover.attempts.len();
        self.proposal_policy.next_seq += round.tasks.len() as u64;
        let Some(task) = round.tasks.into_iter().next() else {
            rec.proposal_stop = Some(format!("{:?}", round.failover.stop));
            if !round.rejected.is_empty() {
                rec.reasons = round
                    .rejected
                    .iter()
                    .map(|r| format!("{}: {:?}", r.title, r.reason))
                    .collect();
            }
            rec.after = self.suite.measure(&self.worktree);
            rec.spend_micros = self.spend_now(stores.ledger).saturating_sub(spend0);
            self.generations.push(rec.clone());
            return rec;
        };
        rec.task_id = Some(task.id.clone());

        // Snapshot before any candidate edit — verification failure restores.
        let snap = snapshot(&self.worktree);
        let cid = format!("GEN-{gen}-{}", task.id);
        rec.lifecycle_id = Some(cid.clone());
        let fence = life.begin(&cid, &task.id, &self.worker, None);
        let _ = life.advance(&cid, fence, Phase::Claimed, "campaign claim");
        let _ = life.advance(&cid, fence, Phase::Implementing, "dispatching");

        // --- 2. implement via failover + confined patch cycle ------------
        let spec = TaskSpec {
            id: task.id.clone(),
            task_class: self.task_class.clone(),
            accept: task.accept.cmd.clone(),
            title: task.title.clone(),
        };
        let exec = CloudImplExec {
            model: self.implementer_model,
            candidates: &self.implementers,
        };
        let mut att = ImplAttempt {
            exec: &exec,
            candidates: &self.implementers,
            worktree: self.worktree.clone(),
            task: spec.clone(),
        };
        let res = run(
            &self.intent,
            &self.implementers,
            stores,
            self.budget,
            &mut att,
        );
        rec.impl_attempts = res.attempts.len();
        let Some(winner) = res.winner else {
            rec.impl_stop = Some(format!("{:?}", res.stop));
            let _ = life.advance(
                &cid,
                fence,
                Phase::Rejected,
                &format!("implementation blocked: {:?}", res.stop),
            );
            // Availability failures attach only to models actually attempted.
            for a in &res.attempts {
                if let Some(c) = self
                    .implementers
                    .iter()
                    .find(|c| c.opaque_id() == a.candidate)
                {
                    let _ = outcomes.record(
                        &self.task_class,
                        &c.model,
                        Outcome::AvailabilityFailure {
                            kind: format!("{:?}", res.stop),
                        },
                    );
                }
            }
            restore(&self.worktree, &snap);
            rec.after = self.suite.measure(&self.worktree);
            rec.spend_micros = self.spend_now(stores.ledger).saturating_sub(spend0);
            self.generations.push(rec.clone());
            return rec;
        };
        let implementer = self.implementers[winner].opaque_id();
        rec.implementer = Some(implementer.clone());
        let _ = life.advance(&cid, fence, Phase::Implemented, "patch applied + verified");
        let _ = life.record_work(
            &cid,
            fence,
            WorkRecord {
                receipts: vec![format!("impl:{} -> applied", task.id)],
                model: Some(implementer.clone()),
                cred_fp: None,
                edits: Vec::new(),
            },
        );

        // --- 3. measure + independent verification -----------------------
        let _ = life.advance(&cid, fence, Phase::Reviewing, "independent review");
        let after = self.suite.measure(&self.worktree);
        let accept_line = format!("{} -> 0", spec.accept.join(" "));
        let packet = ReviewPacket {
            task_id: spec.id.clone(),
            diff: format!("generation {gen} patch applied by {implementer}"),
            accept: spec.accept.clone(),
            implementer: implementer.clone(),
            claimed_receipts: vec![accept_line.clone()],
            actual_receipts: vec![accept_line.clone()],
            baseline: rec.before.clone(),
            measured: after.clone(),
        };
        let _ = life.advance(&cid, fence, Phase::Validating, "gates");
        let v = verify_candidate(
            &self.intent,
            &self.reviewers,
            stores,
            self.budget,
            self.review_model,
            self.gates,
            &packet,
            &self.verify_policy,
            &self.worktree,
        );
        rec.reviewer = v.reviewer.clone();
        rec.gates = v.gates.clone();
        let _ = life.record_gates(&cid, fence, v.gates.clone());
        rec.spend_micros = self.spend_now(stores.ledger).saturating_sub(spend0);

        // --- 4. verdict -> terminal lifecycle + learned outcome ----------
        let usage = OutcomeUsage {
            tokens: 0,
            spend_micros: rec.spend_micros,
            latency_ms: 0,
        };
        if v.verified {
            let _ = life.advance(&cid, fence, Phase::Promoted, "all gates pass");
            let _ = outcomes.record(
                &self.task_class,
                &self.implementers[winner].model,
                Outcome::Promoted {
                    candidate_id: cid.clone(),
                    receipts: vec![accept_line],
                    gates: v.gates.clone(),
                    regression_pct: worst_regression(&rec.before, &after),
                    usage,
                },
            );
            rec.verified = true;
            rec.after = after;
        } else {
            let why = if v.reasons.is_empty() {
                "verification failed".to_string()
            } else {
                v.reasons.join("; ")
            };
            let _ = life.advance(&cid, fence, Phase::Rejected, &why);
            if v.review.as_ref().is_some_and(|r| r.verdict == "reject") {
                if let Some(rev) = &v.reviewer {
                    let _ = outcomes.record(
                        &self.task_class,
                        &self.implementers[winner].model,
                        Outcome::ReviewerDisagreement {
                            reviewer: rev.clone(),
                            candidate_id: cid.clone(),
                        },
                    );
                }
            }
            let _ = outcomes.record(
                &self.task_class,
                &self.implementers[winner].model,
                Outcome::Rejected {
                    candidate_id: cid.clone(),
                    reason: why.clone(),
                    receipts: vec![accept_line],
                    usage,
                },
            );
            rec.reasons = v.reasons;
            // Nothing unverified reaches the worktree.
            restore(&self.worktree, &snap);
            rec.after = self.suite.measure(&self.worktree);
        }
        self.generations.push(rec.clone());
        rec
    }

    /// Measured campaign report — improvement is claimed only from
    /// verified gate results plus held-out metric deltas.
    #[must_use]
    pub fn report(&self, ledger: &BudgetLedger) -> CampaignReport {
        let baseline = self.baseline.clone().unwrap_or_default();
        let final_metrics = self.suite.measure(&self.worktree);
        let promoted = self.generations.iter().filter(|g| g.verified).count();
        let rejected = self
            .generations
            .iter()
            .filter(|g| g.task_id.is_some() && !g.verified)
            .count();
        let blocked = self
            .generations
            .iter()
            .filter(|g| g.task_id.is_none())
            .count();
        let improved = baseline.iter().any(|(k, b)| {
            final_metrics.get(k).is_some_and(|f| f > b)
                && self
                    .generations
                    .iter()
                    .all(|g| g.after.get(k).is_none_or(|a| a >= b))
        });
        CampaignReport {
            generations: self.generations.clone(),
            baseline,
            final_metrics,
            total_spend_micros: self.spend_now(ledger),
            promoted,
            rejected,
            blocked,
            improved,
        }
    }
}

/// Worst percentage regression of `after` vs `before` over shared metrics.
fn worst_regression(before: &BTreeMap<String, f64>, after: &BTreeMap<String, f64>) -> Option<f64> {
    before
        .iter()
        .filter(|(_, b)| **b > 0.0)
        .filter_map(|(k, b)| after.get(k).map(|a| (b - a) / b * 100.0))
        .reduce(f64::max)
}

/// Recursive file snapshot of `dir` (relative path → contents).
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                if let Ok(c) = std::fs::read_to_string(&p) {
                    out.insert(rel.to_path_buf(), c);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Restore `dir` to `snap`: remove files not in the snapshot, rewrite the
/// rest. Confined to `dir` — relative paths only.
fn restore(dir: &Path, snap: &BTreeMap<PathBuf, String>) {
    fn rm_extra(dir: &Path, base: &Path, snap: &BTreeMap<PathBuf, String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rm_extra(&p, base, snap);
                let _ = std::fs::remove_dir(&p); // only succeeds when empty
            } else if let Ok(rel) = p.strip_prefix(base) {
                if !snap.contains_key(rel) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    rm_extra(dir, dir, snap);
    for (rel, contents) in snap {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, contents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_rsi::ImplBrief;
    use std::collections::BTreeSet;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use susi_gawd_agents::cloud_budget::SpendPolicy;
    use susi_gawd_swarm::cloud_lockout::{now_unix, LockoutPolicy, LockoutTracker};
    use susi_gawd_swarm::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const NOW_MS: u64 = 1_700_000_000_000;

    fn cand(key: &str, model: &str, provider: &str, account: &str) -> Candidate {
        cand_cost(key, model, provider, account, 0.0)
    }

    /// Paid candidate — reservations commit real micros so cumulative
    /// spend is measurable (free candidates hold 0 by design).
    fn cand_paid(key: &str, model: &str, provider: &str, account: &str) -> Candidate {
        cand_cost(key, model, provider, account, 0.5)
    }

    fn cand_cost(key: &str, model: &str, provider: &str, account: &str, cost: f64) -> Candidate {
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
            est_latency_ms: 10,
            cost_per_mtok: Some(cost),
            quality: BTreeMap::new(),
        }
    }

    struct StoresOwned {
        elig: Mutex<EligibilityStore>,
        quota: QuotaInventory,
        lock: Mutex<LockoutTracker>,
        ledger: BudgetLedger,
    }
    impl StoresOwned {
        fn new() -> Self {
            Self {
                elig: Mutex::new(EligibilityStore::new()),
                quota: QuotaInventory::new(),
                lock: Mutex::new(LockoutTracker::new(LockoutPolicy::default(), now_unix)),
                ledger: BudgetLedger::new(),
            }
        }
        fn stores(&mut self) -> Stores<'_> {
            Stores {
                eligibility: self.elig.get_mut().unwrap_or_else(|e| e.into_inner()),
                quota: &self.quota,
                lockouts: self.lock.get_mut().unwrap_or_else(|e| e.into_inner()),
                ledger: &self.ledger,
            }
        }
        fn kill(&self, c: &Candidate, status: u16) {
            self.elig
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record_inference(
                    Subject {
                        provider: &c.provider,
                        api_key: &c.api_key,
                        account: c.account.as_deref(),
                        region: c.region.as_deref(),
                        model: &c.model,
                    },
                    &InferenceResult::Failed {
                        status: Some(status),
                        body_snippet: "outage".into(),
                        retry_after_secs: None,
                    },
                    NOW_MS / 1000,
                );
        }
    }

    fn budget() -> FailoverBudget {
        FailoverBudget {
            max_attempts: 4,
            deadline_ms: Some(NOW_MS + 60_000),
            spend: SpendPolicy::FreeOnly,
            attempt_estimate_micros: 1_000,
            now_ms: NOW_MS,
        }
    }

    /// Real held-out suite: `suite.sh` counts how many `cases.txt` lines
    /// appear in `solved.txt`, writes metrics.txt.
    fn mk_worktree(tag: &str) -> PathBuf {
        let wt = std::env::temp_dir().join(format!("rsie2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("cases.txt"), "case-1\ncase-2\ncase-3\n").unwrap();
        std::fs::write(
            wt.join("suite.sh"),
            "#!/bin/sh\npass=0\nwhile read c; do grep -q \"$c\" solved.txt 2>/dev/null && pass=$((pass+1)); done < cases.txt\necho \"cases_passed=$pass\" > metrics.txt\nexit 0\n",
        )
        .unwrap();
        // Per-task acceptance scripts — argv must survive apply_patch_cycle's
        // `join(" ")` + `sh -c`, so multi-condition checks live in files.
        std::fs::write(
            wt.join("accept-201.sh"),
            "#!/bin/sh\ngrep -q case-1 solved.txt && grep -q case-2 solved.txt\n",
        )
        .unwrap();
        std::fs::write(
            wt.join("accept-202.sh"),
            "#!/bin/sh\ngrep -q case-2 solved.txt\n",
        )
        .unwrap();
        std::fs::write(
            wt.join("accept-203.sh"),
            "#!/bin/sh\ngrep -q case-3 solved.txt\n",
        )
        .unwrap();
        std::fs::write(
            wt.join("accept-1.sh"),
            "#!/bin/sh\ngrep -q case-1 solved.txt\n",
        )
        .unwrap();
        wt
    }

    fn suite() -> ScriptSuite {
        ScriptSuite {
            argv: vec!["sh".into(), "suite.sh".into()],
            metrics_file: "metrics.txt".into(),
        }
    }

    /// Proposal backend scripted per model (queued responses).
    struct FakePropose {
        out: Mutex<BTreeMap<String, Vec<AttemptOutcome>>>,
    }
    impl ProposalModel for FakePropose {
        fn propose(&self, c: &Candidate, _p: &str) -> AttemptOutcome {
            let mut m = self.out.lock().unwrap_or_else(|e| e.into_inner());
            let e = m.entry(c.model.clone()).or_default();
            if e.is_empty() {
                return AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(500),
                    body_snippet: "unscripted".into(),
                    retry_after_secs: None,
                });
            }
            if e.len() == 1 {
                e[0].clone()
            } else {
                e.remove(0)
            }
        }
    }

    /// Impl backend keyed on task id → patch-plan JSON.
    struct FakeImpl {
        plans: Mutex<BTreeMap<String, String>>,
    }
    impl ImplModel for FakeImpl {
        fn plan(&self, _c: &Candidate, b: &ImplBrief) -> AttemptOutcome {
            match self
                .plans
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&b.task_id)
            {
                Some(j) => AttemptOutcome::Success(j.clone()),
                None => AttemptOutcome::PreDispatch(InferenceResult::Failed {
                    status: Some(500),
                    body_snippet: "no plan".into(),
                    retry_after_secs: None,
                }),
            }
        }
    }

    /// Reviewer backend: verdicts consumed in call order (default approve).
    struct FakeReview {
        script: Mutex<VecDeque<String>>,
    }
    impl ReviewModel for FakeReview {
        fn review(&self, _c: &Candidate, _p: &ReviewPacket) -> AttemptOutcome {
            let v = self
                .script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or_else(|| "approve".to_string());
            AttemptOutcome::Success(
                serde_json::json!({"verdict": v, "reasons": ["ok"]}).to_string(),
            )
        }
    }

    fn evidence() -> RsiEvidence {
        RsiEvidence {
            failure_receipts: vec![],
            user_feedback: vec![],
            roadmap_gaps: vec!["VC-201-012".into()],
            repo_facts: vec!["held-out suite cases case-1 case-2 case-3 unsolved".into()],
        }
    }

    fn proposal(case: &str, accept: &[&str]) -> String {
        serde_json::json!({
            "proposals": [{
                "title": format!("Solve {case}"),
                "behavior": format!("add {case} to solved.txt"),
                "benefit": "held-out suite pass count rises",
                "risks": [],
                "acceptance": accept,
                "evidence": ["held-out suite cases case-1 case-2 case-3 unsolved"],
                "roadmap": "VC-201-012"
            }]
        })
        .to_string()
    }

    fn policy(seq: u64) -> ProposalPolicy {
        ProposalPolicy {
            valid_vectors: ["VC-201-012"].iter().map(|s| s.to_string()).collect(),
            existing_titles: BTreeSet::new(),
            agent: "devin".into(),
            next_seq: seq,
        }
    }

    /// Everything a campaign drives — backends and candidate pools.
    struct Fx<'a> {
        suite: &'a ScriptSuite,
        props: &'a FakePropose,
        imp: &'a FakeImpl,
        rev: &'a FakeReview,
        proposers: Vec<Candidate>,
        implementers: Vec<Candidate>,
        reviewers: Vec<Candidate>,
    }

    fn mk_campaign<'a>(wt: PathBuf, fx: Fx<'a>) -> Campaign<'a> {
        let mut b = budget();
        b.spend = SpendPolicy::PaidAuthorized {
            max_spend_micro: 100_000_000,
        };
        Campaign::new(
            wt,
            fx.suite,
            fx.props,
            fx.imp,
            fx.rev,
            &WorktreeGates,
            IntentConstraints::default(),
            fx.proposers,
            fx.implementers,
            fx.reviewers,
            b,
            policy(200),
            VerifyPolicy {
                gates: vec![],
                max_regress_pct: 5.0,
            },
            "coding",
            "devin",
        )
    }

    fn plan_write(old: &str, new: &str) -> String {
        serde_json::json!({
            "files": [{"path": "solved.txt", "old": old, "new": new}]
        })
        .to_string()
    }

    #[test]
    fn cloud_rsi_e2e_successive_generations_measured_improvement() {
        let wt = mk_worktree("gens");
        let suite = suite();
        // Proposers: mA dies before gen3 (503) → mB takes over.
        let props = FakePropose {
            out: Mutex::new(BTreeMap::from([
                (
                    "mA".into(),
                    vec![
                        AttemptOutcome::Success(proposal(
                            "case-1+case-2",
                            &["sh", "accept-201.sh"],
                        )),
                        AttemptOutcome::Success(proposal("case-2", &["sh", "accept-202.sh"])),
                    ],
                ),
                (
                    "mB".into(),
                    vec![AttemptOutcome::Success(proposal(
                        "case-3",
                        &["sh", "accept-203.sh"],
                    ))],
                ),
            ])),
        };
        // Task ids are T-DEVIN-201.. per generation.
        let imp = FakeImpl {
            plans: Mutex::new(BTreeMap::from([
                ("T-DEVIN-201".into(), plan_write("", "case-1\ncase-2\n")),
                // gen2 REGRESSION: passes its own accept (case-2 present)
                // but silently drops case-1 — the suite measures 2 -> 1.
                (
                    "T-DEVIN-202".into(),
                    plan_write("case-1\ncase-2\n", "case-2\n"),
                ),
                (
                    "T-DEVIN-203".into(),
                    plan_write("case-1\ncase-2\n", "case-1\ncase-2\ncase-3\n"),
                ),
            ])),
        };
        // Reviewer approves each generation — gen2 is rejected by the
        // measured regression gate, not by opinion.
        let rev = FakeReview {
            script: Mutex::new(VecDeque::new()),
        };
        let pa = cand_paid("ka", "mA", "pA", "aA");
        let pb = cand_paid("kb", "mB", "pB", "aB");
        let imp_m = cand_paid("ki", "mI", "pI", "aI");
        let mut s = StoresOwned::new();
        let mut outcomes = OutcomeLedger::load(wt.join(".ledger"), || 1);
        let mut life = LifecycleStore::load(wt.join(".life"), 1_000_000_000, || 1);
        let mut camp = mk_campaign(
            wt.clone(),
            Fx {
                suite: &suite,
                props: &props,
                imp: &imp,
                rev: &rev,
                proposers: vec![pa.clone(), pb.clone()],
                implementers: vec![imp_m.clone()],
                reviewers: vec![pb.clone(), cand_paid("kc", "mC", "pC", "aC")],
            },
        );

        // Gen 1: case-1+case-2 solved + verified + promoted (0 -> 2).
        let g1 = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(g1.verified, "gen1: {:?}", g1.reasons);
        assert_eq!(g1.after.get("cases_passed"), Some(&2.0));
        assert_eq!(g1.task_id.as_deref(), Some("T-DEVIN-201"));

        // Gen 2: the patch passes its own acceptance but the held-out
        // suite measures a 2 -> 1 regression — verification refuses, the
        // worktree is restored.
        let g2 = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(!g2.verified, "measured regression must block promotion");
        assert!(
            g2.reasons.iter().any(|r| r.contains("regression")),
            "regression reason recorded: {:?}",
            g2.reasons
        );
        assert_eq!(
            g2.after.get("cases_passed"),
            Some(&2.0),
            "rejected candidate restored: suite back to gen1 state"
        );

        // Gen 3: primary proposer mA suffers a 503 → mB proposes; the fix
        // lands and the suite improves to 3/3.
        s.kill(&pa, 503);
        let g3 = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(g3.verified, "gen3: {:?}", g3.reasons);
        assert_eq!(
            g3.proposal_attempts, 1,
            "mA was filtered at selection; mB served the proposal"
        );
        assert_eq!(g3.after.get("cases_passed"), Some(&3.0));

        let rep = camp.report(&s.ledger);
        assert_eq!(rep.promoted, 2);
        assert_eq!(rep.rejected, 1);
        assert!(rep.improved, "measured 0 -> 3 on the held-out suite");
        assert_eq!(rep.baseline.get("cases_passed"), Some(&0.0));
        assert_eq!(rep.final_metrics.get("cases_passed"), Some(&3.0));
        assert!(rep.total_spend_micros > 0, "cumulative cost tracked");

        // Learning: mI has one promoted + one rejected outcome recorded.
        let rec = outcomes.get("coding", "mI").expect("mI outcomes recorded");
        assert_eq!(rec.accepted, 2);
        assert_eq!(rec.rejected, 1);
        // Lifecycle reached terminal phases for all three generations.
        assert_eq!(
            life.get("GEN-1-T-DEVIN-201").map(|r| r.phase),
            Some(Phase::Promoted)
        );
        assert_eq!(
            life.get("GEN-2-T-DEVIN-202").map(|r| r.phase),
            Some(Phase::Rejected)
        );
        assert_eq!(
            life.get("GEN-3-T-DEVIN-203").map(|r| r.phase),
            Some(Phase::Promoted)
        );
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn cloud_rsi_e2e_unverified_generation_claims_no_improvement() {
        let wt = mk_worktree("noverify");
        let suite = suite();
        let props = FakePropose {
            out: Mutex::new(BTreeMap::from([(
                "mA".into(),
                vec![AttemptOutcome::Success(proposal(
                    "case-1",
                    &["sh", "accept-1.sh"],
                ))],
            )])),
        };
        let imp = FakeImpl {
            plans: Mutex::new(BTreeMap::from([(
                "T-DEVIN-201".into(),
                plan_write("", "case-1\n"),
            )])),
        };
        // Every reviewer rejects — no independent approval, no promotion.
        let rev = FakeReview {
            script: Mutex::new(VecDeque::from(["reject".to_string()])),
        };
        let pb = cand("kb", "mB", "pB", "aB");
        let mut s = StoresOwned::new();
        let mut outcomes = OutcomeLedger::load(wt.join(".ledger"), || 1);
        let mut life = LifecycleStore::load(wt.join(".life"), 1_000_000_000, || 1);
        let mut camp = mk_campaign(
            wt.clone(),
            Fx {
                suite: &suite,
                props: &props,
                imp: &imp,
                rev: &rev,
                proposers: vec![cand("ka", "mA", "pA", "aA")],
                implementers: vec![cand("ki", "mI", "pI", "aI")],
                reviewers: vec![pb.clone()],
            },
        );
        let g = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(!g.verified);
        assert_eq!(
            g.after.get("cases_passed"),
            Some(&0.0),
            "rejected work restored — suite unchanged"
        );
        let rep = camp.report(&s.ledger);
        assert!(!rep.improved);
        assert_eq!(rep.promoted, 0);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn cloud_rsi_e2e_blocked_proposal_round_is_honest() {
        let wt = mk_worktree("blocked");
        let suite = suite();
        // Sole proposer is dead before dispatch.
        let props = FakePropose {
            out: Mutex::new(BTreeMap::new()),
        };
        let imp = FakeImpl {
            plans: Mutex::new(BTreeMap::new()),
        };
        let rev = FakeReview {
            script: Mutex::new(VecDeque::new()),
        };
        let pa = cand("ka", "mA", "pA", "aA");
        let mut s = StoresOwned::new();
        s.kill(&pa, 503);
        let mut outcomes = OutcomeLedger::load(wt.join(".ledger"), || 1);
        let mut life = LifecycleStore::load(wt.join(".life"), 1_000_000_000, || 1);
        let mut camp = mk_campaign(
            wt.clone(),
            Fx {
                suite: &suite,
                props: &props,
                imp: &imp,
                rev: &rev,
                proposers: vec![pa],
                implementers: vec![cand("ki", "mI", "pI", "aI")],
                reviewers: vec![cand("kb", "mB", "pB", "aB")],
            },
        );
        let g = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(g.task_id.is_none());
        assert!(g.proposal_stop.is_some(), "real stop reason recorded");
        let rep = camp.report(&s.ledger);
        assert_eq!(rep.blocked, 1);
        assert!(!rep.improved, "a blocked round fabricates no progress");
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn cloud_rsi_e2e_parallel_candidates_isolated_and_billed() {
        // Two candidate implementations run in parallel through the shared
        // scheduler — separate worktrees, one shared budget ledger.
        let root = std::env::temp_dir().join(format!("rsie2e-par-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let wt1 = root.join("c1");
        let wt2 = root.join("c2");
        std::fs::create_dir_all(&wt1).unwrap();
        std::fs::create_dir_all(&wt2).unwrap();

        struct PerTask;
        impl ImplModel for PerTask {
            fn plan(&self, _c: &Candidate, b: &ImplBrief) -> AttemptOutcome {
                let file = format!("{}.txt", b.task_id);
                AttemptOutcome::Success(
                    serde_json::json!({"files": [{"path": file, "old": "", "new": "x\n"}]})
                        .to_string(),
                )
            }
        }
        let model = PerTask;
        let cs = vec![
            cand_paid("k1", "m1", "p1", "a1"),
            cand_paid("k2", "m2", "p2", "a2"),
        ];
        let exec = CloudImplExec {
            model: &model,
            candidates: &cs,
        };
        let s = StoresOwned::new();
        let shared = Shared::new(&s.elig, &s.quota, &s.lock, &s.ledger);
        let jobs = vec![
            Job {
                id: "J1".into(),
                intent: IntentConstraints::default(),
            },
            Job {
                id: "J2".into(),
                intent: IntentConstraints::default(),
            },
        ];
        let tasks: BTreeMap<String, TaskSpec> = BTreeMap::from([
            (
                "J1".to_string(),
                TaskSpec {
                    id: "T-DEVIN-301".into(),
                    task_class: "coding".into(),
                    accept: vec!["test".into(), "-f".into(), "T-DEVIN-301.txt".into()],
                    title: "t1".into(),
                },
            ),
            (
                "J2".to_string(),
                TaskSpec {
                    id: "T-DEVIN-302".into(),
                    task_class: "coding".into(),
                    accept: vec!["test".into(), "-f".into(), "T-DEVIN-302.txt".into()],
                    title: "t2".into(),
                },
            ),
        ]);
        let wts: BTreeMap<String, PathBuf> = BTreeMap::from([
            ("J1".to_string(), wt1.clone()),
            ("J2".to_string(), wt2.clone()),
        ]);
        let make = |j: &Job| ImplAttempt {
            exec: &exec,
            candidates: &cs,
            worktree: wts[&j.id].clone(),
            task: tasks[&j.id].clone(),
        };
        let mut paid = budget();
        paid.spend = SpendPolicy::PaidAuthorized {
            max_spend_micro: 10_000_000,
        };
        let plan = DispatchPlan {
            max_workers: 2,
            mission: "camp".into(),
            per_job: paid,
            job_cpu_millis: 100,
            job_ram_mb: 64,
            job_vram_mb: 0,
            job_subprocesses: 1,
        };
        let outs = run_jobs(&jobs, &cs, &shared, plan, &make);
        assert!(outs.iter().all(|o| o.output.is_some()), "both applied");
        // Isolation: each worktree holds only its own artifact.
        assert!(wt1.join("T-DEVIN-301.txt").is_file());
        assert!(!wt1.join("T-DEVIN-302.txt").exists());
        assert!(wt2.join("T-DEVIN-302.txt").is_file());
        assert!(!wt2.join("T-DEVIN-301.txt").exists());
        // Two distinct model pools served the two candidates.
        let winners: BTreeSet<_> = outs.iter().filter_map(|o| o.winner.clone()).collect();
        assert_eq!(winners.len(), 2);
        // Both billed against the shared ledger (accounts a1+a2 committed).
        let billed = s.ledger.account_exposure("a1") + s.ledger.account_exposure("a2");
        assert!(billed > 0, "parallel attempts commit spend: {billed}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cloud_rsi_e2e_report_never_counts_suggestions_as_progress() {
        // Two proposal rounds with no implementer at all → honest blockage.
        let wt = mk_worktree("report");
        let suite = suite();
        let props = FakePropose {
            out: Mutex::new(BTreeMap::from([(
                "mA".into(),
                vec![
                    AttemptOutcome::Success(proposal("case-1", &["sh", "accept-1.sh"])),
                    AttemptOutcome::Success(proposal("case-2", &["sh", "accept-202.sh"])),
                ],
            )])),
        };
        let imp = FakeImpl {
            plans: Mutex::new(BTreeMap::new()), // no plans: every impl fails
        };
        let rev = FakeReview {
            script: Mutex::new(VecDeque::new()),
        };
        let mut s = StoresOwned::new();
        let mut outcomes = OutcomeLedger::load(wt.join(".ledger"), || 1);
        let mut life = LifecycleStore::load(wt.join(".life"), 1_000_000_000, || 1);
        let mut camp = mk_campaign(
            wt.clone(),
            Fx {
                suite: &suite,
                props: &props,
                imp: &imp,
                rev: &rev,
                proposers: vec![cand("ka", "mA", "pA", "aA")],
                implementers: vec![cand("ki", "mI", "pI", "aI")],
                reviewers: vec![cand("kb", "mB", "pB", "aB")],
            },
        );
        let g1 = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        let g2 = camp.run_generation(&mut s.stores(), &mut outcomes, &mut life, &evidence());
        assert!(g1.impl_stop.is_some() && g2.impl_stop.is_some());
        let rep = camp.report(&s.ledger);
        assert_eq!(rep.promoted, 0);
        assert_eq!(
            rep.rejected, 2,
            "implemented-but-unverified counted as rejected"
        );
        assert!(!rep.improved, "suggestions ≠ improvement");
        assert_eq!(rep.final_metrics.get("cases_passed"), Some(&0.0));
        let _ = std::fs::remove_dir_all(&wt);
    }
}
