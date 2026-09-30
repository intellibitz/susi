//! Brain-coordinated parallel specialist work (T-CODEX-27 / VC-201-013).
//!
//! The strongest working cloud brain plans a mission, specialist
//! model-backed workers execute the jobs **in parallel** (through
//! [`crate::parallel_dispatch::run_jobs`], which enforces shared
//! quota/admission pools), and the brain synthesizes the results. Each
//! coordinator step runs through [`run_step`](crate::brain_supervisor::run_step),
//! so a coordinator that dies mid-mission fails over with the full
//! transition audit trail — coordination is not a single point of
//! failure and never serializes the specialist jobs through the brain.

use susi_gawd_agents::cloud_intent::Candidate;

use crate::brain_supervisor::{run_step, BrainMission, StepOutcome, StepSpec};
use crate::cloud_failover::{Runner, Stores};

/// Which coordination phase a report describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordPhase {
    /// Brain plans the assignments.
    Plan,
    /// Specialists execute in parallel.
    Work,
    /// Brain synthesizes worker outputs.
    Synthesize,
}

/// Outcome of one coordinated mission.
#[derive(Debug)]
pub struct CoordinationReport {
    /// The brain's plan text, if planning completed.
    pub plan: Option<String>,
    /// Per-job receipts returned by the specialist runner.
    pub work_receipts: Vec<String>,
    /// The brain's synthesis, if it completed.
    pub synthesis: Option<String>,
    /// Opaque ids of brains used per phase, in order — provenance for the
    /// audit trail.
    pub brains_used: Vec<String>,
    /// If a phase could not run, which one and why — honest blockage.
    pub blocked: Option<(CoordPhase, String)>,
}

/// A coordinator binds the live evidence stores, candidate inventory and
/// step policy once, then runs coordinated missions.
pub struct Coordinator<'a> {
    /// Live shared evidence (eligibility/quota/lockouts/budget ledger).
    pub stores: &'a mut Stores<'a>,
    /// Candidate inventory.
    pub candidates: &'a [Candidate],
    /// Coordinator step spec (intent, dispatch bounds, policy).
    pub spec: StepSpec<'a>,
}

impl Coordinator<'_> {
    /// Run a coordinated mission: `Plan` → `work(plan, stores)` (the caller
    /// supplies the parallel specialist execution, typically `run_jobs`) →
    /// `Synthesize`.
    ///
    /// The closure owns specialist parallelism — the coordinator only sees
    /// the plan going in and receipts coming out, so independent jobs never
    /// queue behind the brain. It receives the live `Stores` so
    /// worker-observed failures update the same evidence the coordinator's
    /// next step consults. A dead coordinator fails over between or within
    /// steps via the supervisor; an all-blocked coordinator reports
    /// honestly.
    #[must_use]
    pub fn coordinate<R, F>(
        &mut self,
        mission: &mut BrainMission,
        coord: &mut R,
        work: F,
    ) -> CoordinationReport
    where
        R: Runner,
        F: FnOnce(&str, &mut Stores<'_>) -> Vec<String>,
    {
        let mut rep = CoordinationReport {
            plan: None,
            work_receipts: Vec::new(),
            synthesis: None,
            brains_used: Vec::new(),
            blocked: None,
        };
        match run_step(mission, self.stores, self.candidates, &self.spec, coord) {
            StepOutcome::Completed { brain, output } => {
                rep.brains_used.push(brain);
                rep.plan = Some(output);
            }
            StepOutcome::Blocked { reason } => {
                rep.blocked = Some((CoordPhase::Plan, reason));
                return rep;
            }
        }
        // Independent specialist jobs — caller's parallel machinery over
        // the same live evidence stores.
        rep.work_receipts = work(rep.plan.as_deref().unwrap_or_default(), self.stores);
        match run_step(mission, self.stores, self.candidates, &self.spec, coord) {
            StepOutcome::Completed { brain, output } => {
                rep.brains_used.push(brain);
                rep.synthesis = Some(output);
            }
            StepOutcome::Blocked { reason } => {
                rep.blocked = Some((CoordPhase::Synthesize, reason));
            }
        }
        rep
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_failover::AttemptOutcome;
    use crate::cloud_lockout::LockoutTracker;
    use crate::parallel_dispatch::{run_jobs, Job, Shared};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::IntentConstraints;
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

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

    /// Coordinator runner: scripted outcomes keyed by call sequence; the
    /// returned output carries the dispatched candidate index so tests can
    /// prove which brain answered.
    struct Coord {
        calls: Vec<usize>,
        /// call_number -> scripted outcome (overrides the default success)
        fail_at_call: BTreeMap<usize, AttemptOutcome>,
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

    /// Specialist runner: barrier-synchronized to prove real overlap.
    struct Worker {
        idx: usize,
        barrier: Arc<Barrier>,
        in_flight: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }
    impl Runner for Worker {
        fn attempt(&mut self, _index: usize, _dl: u64) -> AttemptOutcome {
            let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(n, Ordering::SeqCst);
            self.barrier.wait();
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            AttemptOutcome::Success(format!("job-{}", self.idx))
        }
    }

    /// Coordinator evidence lives in `Fx`; specialists use their own
    /// mutex-guarded stores, exactly like a real deployment where workers
    /// share replicated state, not the coordinator's memory.
    struct Fx {
        elig: EligibilityStore,
        quota: QuotaInventory,
        lock: LockoutTracker,
        ledger: BudgetLedger,
    }
    impl Fx {
        fn stores(&mut self) -> Stores<'_> {
            Stores {
                eligibility: &mut self.elig,
                quota: &self.quota,
                lockouts: &mut self.lock,
                ledger: &self.ledger,
            }
        }
    }

    fn fx() -> Fx {
        Fx {
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            lock: LockoutTracker::default(),
            ledger: BudgetLedger::new(),
        }
    }

    fn spec(intent: &IntentConstraints) -> StepSpec<'_> {
        StepSpec {
            intent,
            budget: crate::cloud_failover::FailoverBudget {
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

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            ..Default::default()
        }
    }

    /// Plan → N overlapping specialist jobs → synthesis, all coordinated
    /// by the strongest working brain.
    #[test]
    fn powerful_cloud_brain_parallel_plan_overlap_synthesize() {
        let mut f = fx();
        let brain = cand("acme", "sk-brain", "m-brain");
        let w1 = cand("beta", "sk-1", "m-w1");
        let w2 = cand("gamma", "sk-2", "m-w2");
        let w3 = cand("delta", "sk-3", "m-w3");
        f.elig
            .record_inference(subj(&brain), &InferenceResult::Success, T0);
        let candidates = vec![brain];
        let workers = vec![w1, w2, w3];

        // Specialist stores: separate mutex-guarded instances.
        let w_elig = Mutex::new(EligibilityStore::new());
        let w_lock = Mutex::new(LockoutTracker::default());
        let w_ledger = BudgetLedger::new();
        let w_quota = QuotaInventory::new();
        {
            let mut e = w_elig.lock().unwrap_or_else(|x| x.into_inner());
            for w in &workers {
                e.record_inference(subj(w), &InferenceResult::Success, T0);
            }
        }
        let shared = Shared::new(&w_elig, &w_quota, &w_lock, &w_ledger);
        let mut mission = BrainMission::default();
        let mut coord = Coord {
            calls: Vec::new(),
            fail_at_call: BTreeMap::new(),
        };
        let barrier = Arc::new(Barrier::new(3));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let w = workers.clone();
        let int = intent();
        let mut cd = Coordinator {
            stores: &mut f.stores(),
            candidates: &candidates,
            spec: spec(&int),
        };
        let rep = cd.coordinate(&mut mission, &mut coord, |plan, _stores| {
            assert!(plan.starts_with("brain-"));
            let jobs: Vec<Job> = (0..3)
                .map(|i| Job {
                    id: format!("{i}"),
                    intent: intent(),
                })
                .collect();
            let plan_cfg = crate::parallel_dispatch::DispatchPlan {
                max_workers: 3,
                mission: "m".into(),
                per_job: crate::cloud_failover::FailoverBudget {
                    max_attempts: 1,
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
            };
            let (b, inf, mx) = (barrier.clone(), in_flight.clone(), max_seen.clone());
            let outs = run_jobs(&jobs, &w, &shared, plan_cfg, &|job: &Job| Worker {
                idx: job.id.parse::<usize>().unwrap_or(9),
                barrier: b.clone(),
                in_flight: inf.clone(),
                max_seen: mx.clone(),
            });
            outs.iter()
                .map(|o| format!("{}:{:?}", o.job_id, o.output.is_some()))
                .collect()
        });
        assert!(rep.blocked.is_none());
        assert!(rep.plan.is_some() && rep.synthesis.is_some());
        assert_eq!(rep.work_receipts.len(), 3);
        // Real overlap: all three specialists were in flight together.
        assert_eq!(max_seen.load(Ordering::SeqCst), 3);
        // Both coordinator steps hit the brain — real inference, not a
        // recorded preference.
        assert_eq!(coord.calls, vec![0, 0]);
        assert_eq!(rep.brains_used.len(), 2);
    }

    /// Coordinator brain dies between phases (worker evidence records the
    /// death into the shared stores); the supervisor fails over for the
    /// synthesis step and the mission completes with a truthful log.
    #[test]
    fn powerful_cloud_brain_parallel_coordinator_failover_continues() {
        let mut f = fx();
        let a = cand("acme", "sk-a", "m-a");
        let b = cand("beta", "sk-b", "m-b");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        let candidates = vec![a, b];
        let mut mission = BrainMission::default();
        let mut coord = Coord {
            calls: Vec::new(),
            fail_at_call: BTreeMap::new(),
        };
        let int = intent();
        let mut cd = Coordinator {
            stores: &mut f.stores(),
            candidates: &candidates,
            spec: spec(&int),
        };
        let rep = cd.coordinate(&mut mission, &mut coord, |_plan, stores| {
            // Specialists observe the coordinator brain die mid-mission
            // and record it into shared evidence.
            stores.eligibility.record_inference(
                Subject {
                    provider: "acme",
                    api_key: "sk-a",
                    account: None,
                    region: None,
                    model: "m-a",
                },
                &InferenceResult::Failed {
                    status: Some(503),
                    body_snippet: "down".into(),
                    retry_after_secs: None,
                },
                T0 + 5,
            );
            vec!["j0:done".into(), "j1:done".into()]
        });
        assert!(rep.blocked.is_none());
        assert!(rep.synthesis.is_some());
        // Synthesis ran on the fallback brain.
        assert!(rep.brains_used[1].contains("m-b"));
        assert!(mission.transitions.iter().any(|t| matches!(
            t.reason,
            crate::brain_supervisor::TransitionReason::BrainFailed { .. }
        )));
    }

    /// Mid-step coordinator failure — the same step fails over, no replay
    /// of uncertain side effects, mission completes.
    #[test]
    fn powerful_cloud_brain_parallel_midstep_coordinator_death() {
        let mut f = fx();
        let a = cand("acme", "sk-a", "m-a");
        let b = cand("beta", "sk-b", "m-b");
        f.elig
            .record_inference(subj(&a), &InferenceResult::Success, T0);
        f.elig
            .record_inference(subj(&b), &InferenceResult::Success, T0);
        let candidates = vec![a, b];
        let mut mission = BrainMission::default();
        let mut coord = Coord {
            calls: Vec::new(),
            fail_at_call: BTreeMap::new(),
        };
        // Second coordinator dispatch (synthesis) fails pre-dispatch.
        coord.fail_at_call.insert(
            1,
            AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: Some(503),
                body_snippet: "down".into(),
                retry_after_secs: None,
            }),
        );
        let int = intent();
        let mut cd = Coordinator {
            stores: &mut f.stores(),
            candidates: &candidates,
            spec: spec(&int),
        };
        let rep = cd.coordinate(&mut mission, &mut coord, |_plan, _stores| vec!["ok".into()]);
        // Plan succeeded on A; synthesis attempt on A failed, failed over
        // to B within the same step.
        assert!(rep.blocked.is_none());
        assert!(rep.synthesis.as_deref().unwrap_or("").contains("brain-1"));
        assert!(rep.brains_used[1].contains("m-b"));
    }

    /// No working brain: the mission is honestly blocked at Plan and the
    /// specialist work closure is never invoked.
    #[test]
    fn powerful_cloud_brain_parallel_all_dead_blocks_before_work() {
        let mut f = fx();
        let dead = cand("acme", "sk-d", "m-dead");
        f.elig.record_inference(
            subj(&dead),
            &InferenceResult::Failed {
                status: Some(401),
                body_snippet: "revoked".into(),
                retry_after_secs: None,
            },
            T0,
        );
        let candidates = vec![dead];
        let mut mission = BrainMission::default();
        let mut coord = Coord {
            calls: Vec::new(),
            fail_at_call: BTreeMap::new(),
        };
        let invoked = std::cell::Cell::new(false);
        let int = intent();
        let mut cd = Coordinator {
            stores: &mut f.stores(),
            candidates: &candidates,
            spec: spec(&int),
        };
        let rep = cd.coordinate(&mut mission, &mut coord, |_plan, _stores| {
            invoked.set(true);
            Vec::new()
        });
        assert_eq!(rep.blocked.map(|b| b.0), Some(CoordPhase::Plan));
        assert!(
            !invoked.get(),
            "specialist work must not run when planning is blocked"
        );
    }
}
