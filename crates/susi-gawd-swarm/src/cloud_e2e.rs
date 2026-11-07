//! Hermetic fake-provider end-to-end harness for the cloud dispatch path.
//!
//! The production loop is exercised end-to-end: `cloud_intent::select`
//! filters/ranks against live eligibility + quota evidence, admission and
//! budget gates run, `Runner::attempt` performs the (scripted) provider
//! call, and every outcome is written back into the shared stores — so a
//! fake "healthy" health probe that then fails inference updates state
//! exactly like the real HTTP provider does.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::cloud_failover::AttemptOutcome;

/// A fake provider: per-candidate queue of scripted outcomes, shared
/// between the workers of a parallel dispatch (the same provider answers
/// every job). `attempt(i)` pops the next scripted outcome for candidate
/// `i`; an exhausted script fails 503 — unknown behavior is failure, not
/// success.
#[derive(Clone)]
pub struct FakeProvider {
    plans: Arc<Mutex<Vec<VecDeque<AttemptOutcome>>>>,
    /// Every index actually attempted — the dispatch audit trail.
    pub calls: Arc<Mutex<Vec<usize>>>,
}

impl FakeProvider {
    /// Build from per-candidate scripts (indexed parallel to `candidates`).
    pub fn new(plans: Vec<Vec<AttemptOutcome>>) -> Self {
        Self {
            plans: Arc::new(Mutex::new(plans.into_iter().map(VecDeque::from).collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl crate::cloud_failover::Runner for FakeProvider {
    fn attempt(&mut self, index: usize, _remaining_ms: u64) -> AttemptOutcome {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(index);
        self.plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(index)
            .and_then(|q| q.pop_front())
            .unwrap_or_else(|| {
                AttemptOutcome::PreDispatch(
                    susi_vendor_models::cloud_eligibility::InferenceResult::Failed {
                        status: Some(503),
                        body_snippet: "unscripted".into(),
                        retry_after_secs: None,
                    },
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult, Subject};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    use crate::cloud_e2e::FakeProvider;
    use crate::cloud_failover::{run, FailoverBudget, Stores};
    use crate::cloud_lockout::LockoutTracker;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    /// Ten credentials across vendors + shared accounts, mixed states.
    /// idx: 0 good, 1 good same-acct, 2 invalid, 3 rate-limited,
    /// 4 quota-exhausted, 5 model-denied, 6 outage, 7 unknown, 8 free,
    /// 9 paid-only.
    fn ten_keys() -> Vec<Candidate> {
        let mut v = Vec::new();
        let specs = [
            ("k0", "m0", "prov-a", Some("acct-a"), 1.0),
            ("k1", "m1", "prov-a", Some("acct-a"), 1.0),
            ("k2", "m2", "prov-b", Some("acct-b"), 1.0),
            ("k3", "m3", "prov-c", Some("acct-c"), 1.0),
            ("k4", "m4", "prov-d", Some("acct-d"), 1.0),
            ("k5", "m5", "prov-e", Some("acct-e"), 1.0),
            ("k6", "m6", "prov-f", Some("acct-f"), 1.0),
            ("k7", "m7", "prov-g", Some("acct-g"), 1.0),
            ("k8", "m8", "prov-h", Some("acct-h"), 0.0),
            ("k9", "m9", "prov-i", Some("acct-i"), 5.0),
        ];
        for (k, m, p, a, cost) in specs {
            v.push(Candidate {
                provider: p.into(),
                api_key: k.into(),
                account: a.map(Into::into),
                region: None,
                model: m.into(),
                context_tokens: 128_000,
                modalities: vec![],
                supports_tools: true,
                supports_structured_output: true,
                residency: None,
                est_latency_ms: 200,
                cost_per_mtok: Some(cost),
                quality: BTreeMap::new(),
            });
        }
        v
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

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: None,
        }
    }

    fn seed_fail(
        cs: &[Candidate],
        store: &mut EligibilityStore,
        idx: usize,
        status: u16,
        body: &str,
    ) {
        store.record_inference(subj(&cs[idx]), &fail(status, body), T0);
    }

    fn seed_ok(cs: &[Candidate], store: &mut EligibilityStore, idx: usize) {
        store.record_inference(subj(&cs[idx]), &InferenceResult::Success, T0);
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 2,
            ..Default::default()
        }
    }

    fn fbudget(spend: SpendPolicy) -> FailoverBudget {
        FailoverBudget {
            max_attempts: 12,
            deadline_ms: Some(T0_MS + 120_000),
            spend,
            attempt_estimate_micros: 10,
            now_ms: T0_MS,
        }
    }

    struct Env<'a> {
        stores: Stores<'a>,
    }

    fn env<'a>(
        elig: &'a mut EligibilityStore,
        quota: &'a QuotaInventory,
        lock: &'a mut LockoutTracker,
        ledger: &'a BudgetLedger,
    ) -> Env<'a> {
        Env {
            stores: Stores {
                eligibility: elig,
                quota,
                lockouts: lock,
                ledger,
            },
        }
    }

    /// Scripted plan: mostly 503, `ok` at the listed indexes.
    fn scripts(n: usize, ok: &[usize]) -> Vec<Vec<AttemptOutcome>> {
        let mut v: Vec<Vec<AttemptOutcome>> = (0..n)
            .map(|_| vec![AttemptOutcome::PreDispatch(fail(503, "down"))])
            .collect();
        for &i in ok {
            v[i] = vec![AttemptOutcome::Success(format!("answer-{i}"))];
        }
        v
    }

    #[test]
    fn cloud_ten_key_intent_e2e_dispatch_completes_on_eligible_model() {
        // Realistic mixed-availability state: only k0, k1, k8 proven working;
        // the intent completes on an eligible working model.
        let cs = ten_keys();
        let mut elig = EligibilityStore::new();
        seed_ok(&cs, &mut elig, 0);
        seed_ok(&cs, &mut elig, 1);
        seed_ok(&cs, &mut elig, 8);
        seed_fail(&cs, &mut elig, 2, 401, "invalid api key");
        seed_fail(&cs, &mut elig, 3, 429, "rate limited");
        seed_fail(&cs, &mut elig, 4, 402, "quota exceeded");
        seed_fail(&cs, &mut elig, 5, 403, "model access denied");
        seed_fail(&cs, &mut elig, 6, 503, "outage");
        seed_fail(&cs, &mut elig, 9, 402, "insufficient credit");
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        let mut fp = FakeProvider::new(scripts(10, &[0, 1, 8]));
        let out = run(
            &intent(),
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
            &mut fp,
        );
        assert!(out.output.is_some());
        // Every attempt landed on a proven-working index — dead/blocked
        // credentials never dispatched.
        let calls = fp.calls.lock().unwrap().clone();
        assert!(
            calls.iter().all(|i| [0usize, 1, 8].contains(i)),
            "{calls:?}"
        );
        assert!(out.attempts.len() <= calls.len());
    }

    #[test]
    fn cloud_ten_key_intent_e2e_preferred_dead_model_fails_over() {
        // Highest-quality candidate is dead; a lower-ranked compatible one
        // must serve the intent. Selection is not completion.
        let mut cs = ten_keys();
        cs[0].quality.insert("coding".into(), (50, 50)); // preferred
        cs[3].quality.insert("coding".into(), (10, 50));
        let mut elig = EligibilityStore::new();
        seed_ok(&cs, &mut elig, 0);
        seed_ok(&cs, &mut elig, 1);
        seed_ok(&cs, &mut elig, 3);
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        // Provider script: idx 0 (preferred) fails pre-dispatch; 1,3 succeed.
        let mut plans = scripts(10, &[1, 3]);
        plans[0] = vec![AttemptOutcome::PreDispatch(fail(500, "boom"))];
        let mut fp = FakeProvider::new(plans);
        let out = run(
            &intent(),
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
            &mut fp,
        );
        assert!(out.output.is_some());
        assert_ne!(out.winner, Some(0), "dead preferred model must not win");
        // The failure was recorded — re-selecting must not lead with it.
        let sel = susi_gawd_agents::cloud_intent::select(&intent(), &cs, &elig, &quota, T0);
        assert!(sel.rejected.iter().any(|r| r.candidate.contains("m0")));
    }

    #[test]
    fn cloud_ten_key_intent_e2e_model_listing_success_is_not_inference() {
        // A management `/models` probe succeeding must NOT make a model
        // usable: candidate stays `discovery` (unproven), and a failed
        // inference still updates eligibility.
        let cs = ten_keys();
        let mut elig = EligibilityStore::new();
        // "Misleading" probe: management listing ok → records a non-inference
        // observation (management probes never produce Usable).
        elig.record(
            susi_vendor_models::cloud_eligibility::Observation {
                provider: "prov-g".into(),
                credential: susi_vendor_models::cloud_eligibility::credential_fingerprint("k7"),
                account: "acct-g".into(),
                region: String::new(),
                model: "m7".into(),
                kind: susi_vendor_models::cloud_eligibility::EligibilityKind::Usable,
                level: susi_vendor_models::cloud_eligibility::ScopeLevel::Model,
                provenance: susi_vendor_models::cloud_eligibility::Provenance::Probe,
                reason: "models listing ok".into(),
                observed_unix: T0,
                expires_unix: None,
            },
            Some("k7"),
        );
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        // The "healthy" model fails inference anyway.
        let mut plans = scripts(10, &[]);
        plans[7] = vec![AttemptOutcome::PreDispatch(fail(
            500,
            "broken despite listing",
        ))];
        let mut fp = FakeProvider::new(plans);
        let mut i = intent();
        i.pinned_model = Some("m7".into());
        let out = run(
            &i,
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
            &mut fp,
        );
        // Pin with default fallback → fails over to other candidates
        // (they're all 503 here) — m7's attempt evidence is recorded.
        let v = e.stores.eligibility.resolve(subj(&cs[7]), T0);
        assert_ne!(
            v.kind,
            susi_vendor_models::cloud_eligibility::EligibilityKind::Usable,
            "a listing probe must never mark inference usable"
        );
        let _ = out;
    }

    #[test]
    fn cloud_ten_key_intent_e2e_free_quota_never_silently_charges() {
        // Free-only intent: free candidate exhausts → failover does NOT
        // silently spend on paid candidates.
        let cs = ten_keys();
        let mut elig = EligibilityStore::new();
        seed_ok(&cs, &mut elig, 8); // free model working
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        // Free candidate fails; paid candidates would succeed but policy
        // is FreeOnly — they must never be attempted.
        let mut plans = scripts(10, &[0, 1, 9]);
        plans[8] = vec![AttemptOutcome::PreDispatch(fail(503, "free pool down"))];
        let mut fp = FakeProvider::new(plans);
        let mut i = intent();
        i.max_cost_per_mtok = Some(0.0); // free-only
        let _out = run(
            &i,
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::FreeOnly),
            &mut fp,
        );
        // The free model failed; no paid fallback was silently spent.
        let calls = fp.calls.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .all(|&i| i == 8 || cs[i].cost_per_mtok == Some(0.0)),
            "paid candidates dispatched under FreeOnly: {calls:?}"
        );
    }

    #[test]
    fn cloud_ten_key_intent_e2e_parallel_intents_across_accounts() {
        // Two intents run in parallel through run_jobs across distinct
        // pools; each lands on a working model.
        use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};
        let cs = ten_keys();
        let elig = Mutex::new(EligibilityStore::new());
        {
            let mut e = elig.lock().unwrap();
            seed_ok(&cs, &mut e, 0);
            seed_ok(&cs, &mut e, 6);
        }
        let quota = QuotaInventory::new();
        let lock = Mutex::new(LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let shared = Shared::new(&elig, &quota, &lock, &ledger);
        let jobs = vec![
            Job {
                id: "coding".into(),
                intent: intent(),
            },
            Job {
                id: "summary".into(),
                intent: IntentConstraints {
                    task_class: "summary".into(),
                    discovery_budget: 2,
                    ..Default::default()
                },
            },
        ];
        let fp = FakeProvider::new(scripts(10, &[0, 6]));
        let fp2 = fp.clone();
        let make = move |_j: &Job| fp2.clone();
        let plan = DispatchPlan {
            max_workers: 2,
            mission: "e2e".into(),
            job_cpu_millis: 0,
            job_ram_mb: 0,
            job_vram_mb: 0,
            job_subprocesses: 0,
            per_job: fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
        };
        let outcomes = run_jobs(&jobs, &cs, &shared, plan, &make);
        assert!(outcomes.iter().all(|o| o.output.is_some()));
    }

    #[test]
    fn cloud_ten_key_intent_e2e_all_blocked_is_honest_failure() {
        let cs = ten_keys();
        let mut elig = EligibilityStore::new();
        for i in 0..10 {
            seed_fail(&cs, &mut elig, i, 503, "everything down");
        }
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        let mut fp = FakeProvider::new(scripts(10, &[]));
        let mut i = intent();
        i.discovery_budget = 0;
        let out = run(
            &i,
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
            &mut fp,
        );
        assert!(out.output.is_none());
        assert!(out.stop.is_some());
        // No partial-success claim, no spliced answer.
        assert!(out.winner.is_none());
    }

    #[test]
    fn cloud_ten_key_intent_e2e_failure_order_never_strands_a_working_model() {
        // Property-ish: for EVERY single-candidate working position, the
        // intent completes — failure ordering must not matter.
        for working in 0..10 {
            let cs = ten_keys();
            let mut elig = EligibilityStore::new();
            for i in 0..10 {
                seed_ok(&cs, &mut elig, i);
            }
            let quota = QuotaInventory::new();
            let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
            let ledger = BudgetLedger::new();
            let mut e = env(&mut elig, &quota, &mut lock, &ledger);
            let mut plans = scripts(10, &[working]);
            // everyone else fails pre-dispatch on every attempt
            for (i, p) in plans.iter_mut().enumerate() {
                if i != working {
                    *p = (0..16)
                        .map(|_| AttemptOutcome::PreDispatch(fail(503, "down")))
                        .collect();
                }
            }
            let mut fp = FakeProvider::new(plans);
            let out = run(
                &intent(),
                &cs,
                &mut e.stores,
                fbudget(SpendPolicy::PaidAuthorized {
                    max_spend_micro: 10_000,
                }),
                &mut fp,
            );
            assert_eq!(
                out.winner,
                Some(working),
                "working candidate {working} must be found"
            );
        }
    }

    #[test]
    fn cloud_ten_key_intent_e2e_midstream_never_splices() {
        let cs = ten_keys();
        let mut elig = EligibilityStore::new();
        seed_ok(&cs, &mut elig, 0);
        seed_ok(&cs, &mut elig, 1);
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut e = env(&mut elig, &quota, &mut lock, &ledger);
        let mut plans = scripts(10, &[1]);
        plans[0] = vec![AttemptOutcome::MidStream {
            result: fail(500, "stream died"),
            partial: "TRUNCATED-GARBAGE".into(),
        }];
        let mut fp = FakeProvider::new(plans);
        let out = run(
            &intent(),
            &cs,
            &mut e.stores,
            fbudget(SpendPolicy::PaidAuthorized {
                max_spend_micro: 1_000,
            }),
            &mut fp,
        );
        assert_eq!(out.output.as_deref(), Some("answer-1"));
        assert!(!out.output.as_deref().unwrap().contains("GARBAGE"));
    }
}
