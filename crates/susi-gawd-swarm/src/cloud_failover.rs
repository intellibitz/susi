//! Intent failover: connect selection to actual dispatch, bounded and safe.
//!
//! `select()` (susi-gawd-agents) produces a ranked candidate order; this
//! module *executes* it. Selection is not completion — a pinned or highly
//! ranked model that fails must not strand the intent while a compatible
//! working alternative remains.
//!
//! The loop is deliberately small and fully injected:
//! - `Runner` abstracts the real call (tests inject scripted outcomes).
//! - `EligibilityStore` is updated on every attempt (fresh evidence wins).
//! - `LockoutTracker` rate-limits retry of degraded targets.
//! - `BudgetLedger` holds spend per account across attempts and workers.
//! - Attempt, deadline, and spend budgets are hard stops.
//!
//! Failure kinds are handled honestly:
//! - `PreDispatch` — nothing ran; another candidate may be tried freely.
//! - `MidStream` — a partial response was produced; it is discarded, never
//!   spliced into a later candidate's answer.
//! - `Ambiguous` — the call may have completed server-side (external tool
//!   side effects possible); failover continues, but the result is marked
//!   `prior_ambiguous` so the caller never silently treats it as clean.

use susi_gawd_agents::cloud_budget::{BudgetLedger, Denial, FreeHeadroom, SpendPolicy};
use susi_gawd_agents::cloud_intent::{select, Candidate, IntentConstraints, Selection};
use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult};
use susi_vendor_models::cloud_quota::QuotaInventory;

use crate::cloud_lockout::{LockoutTracker, Permit};

/// What happened on a single attempt — injected by the runner.
#[derive(Debug, Clone)]
pub enum AttemptOutcome {
    /// The call succeeded; payload returned.
    Success(String),
    /// Failed before any response — safe to retry elsewhere immediately.
    PreDispatch(InferenceResult),
    /// The worker/executor failed without involving the model (agent
    /// crash, workspace fault). NOT model evidence: never recorded into
    /// eligibility or lockouts, so a broken worker can't poison the pool
    /// for sibling jobs. Safe to retry on the next candidate.
    WorkerFailed(String),
    /// Failed mid-stream; `partial` was produced but is discarded — never
    /// spliced into another model's answer.
    MidStream {
        /// Provider-visible evidence for classification.
        result: InferenceResult,
        /// Discarded partial output (kept only for diagnostics/logging).
        partial: String,
    },
    /// Outcome unknown — the request may have completed server-side, so
    /// external side effects may already have happened.
    Ambiguous(InferenceResult),
}

/// One executed attempt in the failover trace.
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    /// Opaque `provider/model/credfp8` identifier — no key material.
    pub candidate: String,
    /// What happened, classified.
    pub outcome: &'static str,
}

/// Why failover stopped without success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailoverStop {
    /// Selection produced no dispatchable candidates.
    NoEligibleCandidates,
    /// The attempt budget ran out.
    AttemptBudgetExhausted { used: u32 },
    /// The deadline elapsed between attempts.
    DeadlineExceeded,
    /// Every remaining candidate hit the spend policy.
    BudgetExhausted,
    /// Candidates remain but all are locked out / cooling down.
    AllLockedOut,
    /// Admission control refused the job (bounded queue / caps).
    Admission(String),
}

/// The result of a failover run.
#[derive(Debug)]
pub struct FailoverResult {
    /// Winning output, if any attempt succeeded.
    pub output: Option<String>,
    /// Index into `candidates` of the winner.
    pub winner: Option<usize>,
    /// Every attempt made, in order — the audit trail.
    pub attempts: Vec<AttemptRecord>,
    /// At least one `Ambiguous` attempt ran before the winner — external
    /// side effects may exist; the answer is not proven clean.
    pub prior_ambiguous: bool,
    /// Why we stopped, when `output` is `None`.
    pub stop: Option<FailoverStop>,
}

/// The shared stores a failover run reads and writes.
pub struct Stores<'a> {
    /// Per-credential/account/model evidence, updated on every attempt.
    pub eligibility: &'a mut EligibilityStore,
    /// Provider quota evidence consulted during selection.
    pub quota: &'a QuotaInventory,
    /// Scoped cooldown/circuit state consulted and updated per attempt.
    pub lockouts: &'a mut LockoutTracker,
    /// Shared spend ledger — reservations span attempts and workers.
    pub ledger: &'a BudgetLedger,
}

/// Bounds on a failover run.
#[derive(Debug, Clone, Copy)]
pub struct FailoverBudget {
    /// Maximum dispatch attempts across all candidates.
    pub max_attempts: u32,
    /// Wall deadline in ms since epoch (injected clock domain).
    pub deadline_ms: Option<u64>,
    /// Spend policy applied to every attempt.
    pub spend: SpendPolicy,
    /// Per-attempt cost estimate (micros) when pricing is unknown.
    pub attempt_estimate_micros: u64,
    /// Injected clock at run start, same ms domain as `deadline_ms`.
    pub now_ms: u64,
}

/// Injected executor: perform one attempt against `candidates[index]`.
/// Receives ms remaining until the deadline (or `u64::MAX` when none).
/// Implementations must not leak `Candidate.api_key` into outcomes.
pub trait Runner {
    fn attempt(&mut self, index: usize, deadline_remaining_ms: u64) -> AttemptOutcome;
}

/// Run an intent through failover: select, then attempt in rank order
/// until success or a bound is hit. Every attempt updates eligibility and
/// lockout evidence; spend is reserved per attempt and released/commit
/// against the shared ledger.
#[must_use]
pub fn run<R: Runner>(
    intent: &IntentConstraints,
    candidates: &[Candidate],
    stores: &mut Stores<'_>,
    budget: FailoverBudget,
    runner: &mut R,
) -> FailoverResult {
    let selection = select(
        intent,
        candidates,
        stores.eligibility,
        stores.quota,
        budget.now_ms / 1000,
    );
    run_selection(&selection, candidates, stores, budget, runner)
}

/// Execute a precomputed selection — the dispatch half of [`run`].
pub fn run_selection<R: Runner>(
    selection: &Selection,
    candidates: &[Candidate],
    stores: &mut Stores<'_>,
    budget: FailoverBudget,
    runner: &mut R,
) -> FailoverResult {
    let mut now_ms = budget.now_ms;
    let mut out = FailoverResult {
        output: None,
        winner: None,
        attempts: Vec::new(),
        prior_ambiguous: false,
        stop: None,
    };
    if selection.ranked.is_empty() {
        out.stop = Some(FailoverStop::NoEligibleCandidates);
        return out;
    }
    let mut attempted: Vec<usize> = Vec::new();
    let mut saw_lockout_only = false;
    let mut saw_budget_denial = false;
    loop {
        let Some(ranked) = selection.next_after(&attempted) else {
            // Report the *actual* reason nothing else could run — a spend
            // denial is BudgetExhausted, not "we ran out of attempts".
            out.stop = Some(if saw_lockout_only {
                FailoverStop::AllLockedOut
            } else if saw_budget_denial {
                FailoverStop::BudgetExhausted
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
        let scope = ranked.candidate.clone();
        // Lockout gate — a cooling target is skipped, not hammered.
        match stores.lockouts.permit(
            &scope,
            susi_vendor_models::cloud_eligibility::EligibilityKind::Unknown,
        ) {
            Permit::Allowed => {}
            Permit::Cooldown { .. } | Permit::CircuitOpen { .. } | Permit::ProbesExhausted => {
                saw_lockout_only = true;
                attempted.push(ranked.index);
                continue;
            }
            Permit::Permanent { .. } => {
                attempted.push(ranked.index);
                continue;
            }
        }
        // Spend gate — a denied reservation skips this candidate.
        let reservation =
            match stores
                .ledger
                .reserve(susi_gawd_agents::cloud_budget::ReserveRequest {
                    account: c.account.as_deref().unwrap_or(""),
                    credential_fp: &susi_vendor_models::cloud_eligibility::credential_fingerprint(
                        &c.api_key,
                    ),
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
                    saw_budget_denial = true;
                    attempted.push(ranked.index);
                    out.stop = Some(FailoverStop::BudgetExhausted);
                    // Keep scanning: a *free* sibling may still be legal.
                    continue;
                }
            };
        attempted.push(ranked.index);
        let remaining = budget
            .deadline_ms
            .map(|dl| dl.saturating_sub(now_ms))
            .unwrap_or(u64::MAX);
        let outcome = runner.attempt(ranked.index, remaining);
        match outcome {
            AttemptOutcome::Success(text) => {
                stores.eligibility.record_inference(
                    subj(c),
                    &InferenceResult::Success,
                    now_ms / 1000,
                );
                stores.lockouts.record(&scope, &InferenceResult::Success);
                stores.ledger.commit(reservation, None);
                out.output = Some(text);
                out.winner = Some(ranked.index);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "success",
                });
                return out;
            }
            AttemptOutcome::PreDispatch(res) => {
                stores
                    .eligibility
                    .record_inference(subj(c), &res, now_ms / 1000);
                stores.lockouts.record(&scope, &res);
                // Nothing ran: release the hold so retries don't bill twice.
                stores.ledger.release(reservation);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "pre_dispatch",
                });
            }
            AttemptOutcome::WorkerFailed(why) => {
                let _ = why;
                // No model was touched: no evidence, release the hold.
                stores.ledger.release(reservation);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "worker_failed",
                });
            }
            AttemptOutcome::MidStream {
                result: res,
                partial,
            } => {
                // Discard partial output; record the failure honestly.
                let _ = partial;
                stores
                    .eligibility
                    .record_inference(subj(c), &res, now_ms / 1000);
                stores.lockouts.record(&scope, &res);
                stores.ledger.commit(reservation, None);
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "mid_stream",
                });
            }
            AttemptOutcome::Ambiguous(res) => {
                stores
                    .eligibility
                    .record_inference(subj(c), &res, now_ms / 1000);
                stores.lockouts.record(&scope, &res);
                stores.ledger.commit(reservation, None);
                out.prior_ambiguous = true;
                out.attempts.push(AttemptRecord {
                    candidate: ranked.candidate.clone(),
                    outcome: "ambiguous",
                });
            }
        }
        // Advance the injected clock for the next budget/deadline check;
        // runners may also advance it (real time elapsed inside attempt).
        now_ms = now_ms.saturating_add(1);
    }
}

fn subj<'a>(c: &'a Candidate) -> susi_vendor_models::cloud_eligibility::Subject<'a> {
    susi_vendor_models::cloud_eligibility::Subject {
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
    use susi_gawd_agents::cloud_budget::SpendPolicy;
    use susi_gawd_agents::cloud_intent::IntentConstraints;
    use susi_vendor_models::cloud_eligibility::Subject;
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    fn cand(key: &str, model: &str) -> Candidate {
        Candidate {
            provider: "acme".into(),
            api_key: key.into(),
            account: None,
            region: None,
            model: model.into(),
            context_tokens: 128_000,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 800,
            cost_per_mtok: Some(1.0),
            quality: BTreeMap::new(),
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "coding".into(),
            discovery_budget: 4,
            ..Default::default()
        }
    }

    fn budget() -> FailoverBudget {
        FailoverBudget {
            max_attempts: 8,
            deadline_ms: Some(T0_MS + 60_000),
            spend: SpendPolicy::PaidAuthorized {
                max_spend_micro: 10_000,
            },
            attempt_estimate_micros: 50,
            now_ms: T0_MS,
        }
    }

    fn fail(status: u16, body: &str) -> InferenceResult {
        InferenceResult::Failed {
            status: Some(status),
            body_snippet: body.into(),
            retry_after_secs: None,
        }
    }

    /// Scripted runner: index → outcome.
    struct Script(Vec<AttemptOutcome>);
    impl Runner for Script {
        fn attempt(&mut self, index: usize, _remaining: u64) -> AttemptOutcome {
            self.0
                .get(index)
                .cloned()
                .unwrap_or_else(|| AttemptOutcome::PreDispatch(fail(500, "unscripted failure")))
        }
    }

    fn ok_at(working: &[usize]) -> Vec<AttemptOutcome> {
        // default PreDispatch(503), positions in `working` succeed
        let mut v = vec![AttemptOutcome::PreDispatch(fail(503, "down")); 12];
        for &i in working {
            v[i] = AttemptOutcome::Success(format!("answer-from-{i}"));
        }
        v
    }

    fn prove(cs: &[Candidate], store: &mut EligibilityStore, idxs: &[usize]) {
        for &i in idxs {
            store.record_inference(
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

    #[test]
    fn cloud_intent_failover_succeeds_on_working_alternative() {
        // Ranked leader fails; the intent still completes on a working
        // sibling — selection is not completion.
        let mut cs: Vec<Candidate> = (0..5)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        cs[0].quality.insert("coding".into(), (10, 10));
        let mut elig = EligibilityStore::new();
        prove(&cs, &mut elig, &[0, 1, 2]);
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut r = Script(ok_at(&[1]));
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut r,
        );
        assert_eq!(out.winner, Some(1));
        assert_eq!(out.output.as_deref(), Some("answer-from-1"));
        assert_eq!(out.attempts.len(), 2);
    }

    #[test]
    fn cloud_intent_failover_shared_account_block_scopes_candidates() {
        // A 402 on account acct-1 must not strand candidates on acct-2.
        let mut a = cand("sk-a1", "mA");
        a.account = Some("acct-1".into());
        let mut b = cand("sk-b1", "mB");
        b.account = Some("acct-2".into());
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut script = Script(ok_at(&[1]));
        script.0[0] = AttemptOutcome::PreDispatch(fail(402, "insufficient credit"));
        let cs = vec![a, b];
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut r0(script),
        );
        assert_eq!(out.winner, Some(1), "sibling account must still dispatch");
    }
    fn r0(s: Script) -> Script {
        s
    }

    #[test]
    fn cloud_intent_failover_midstream_partial_is_never_spliced() {
        let cs: Vec<Candidate> = (0..3)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        prove(&cs, &mut elig, &[0, 1]);
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut script = Script(ok_at(&[1]));
        script.0[0] = AttemptOutcome::MidStream {
            result: fail(500, "stream reset"),
            partial: "garbage-half-answer".into(),
        };
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut script,
        );
        assert_eq!(out.output.as_deref(), Some("answer-from-1"));
        assert!(!out.output.as_deref().unwrap().contains("garbage"));
    }

    #[test]
    fn cloud_intent_failover_ambiguous_marks_side_effects() {
        let cs: Vec<Candidate> = (0..3)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut script = Script(ok_at(&[1]));
        script.0[0] = AttemptOutcome::Ambiguous(fail(500, "timeout mid-call"));
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut script,
        );
        assert_eq!(out.winner, Some(1));
        assert!(
            out.prior_ambiguous,
            "possible side effects must be surfaced"
        );
    }

    #[test]
    fn cloud_intent_failover_exhaustion_returns_typed_error() {
        let cs: Vec<Candidate> = (0..3)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut script = Script(ok_at(&[]));
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut script,
        );
        assert!(out.output.is_none());
        assert!(matches!(
            out.stop,
            Some(FailoverStop::AttemptBudgetExhausted { .. }) | Some(FailoverStop::AllLockedOut)
        ));
        // Failed attempts updated eligibility — the same intent run again
        // must not re-lead the dead keys.
        let sel = select(&intent(), &cs, &elig, &quota, T0);
        assert!(sel.ranked.iter().all(|r| r.discovery));
    }

    #[test]
    fn cloud_intent_failover_deadline_stops_attempts() {
        let cs: Vec<Candidate> = (0..5)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        // Runner consumes 30s per attempt; deadline is 40s → at most 1-2 tries.
        struct Slow;
        impl Runner for Slow {
            fn attempt(&mut self, _i: usize, remaining: u64) -> AttemptOutcome {
                let _ = remaining;
                AttemptOutcome::PreDispatch(fail(503, "down"))
            }
        }
        let mut b = budget();
        b.deadline_ms = Some(T0_MS + 1);
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            b,
            &mut Slow,
        );
        assert!(matches!(out.stop, Some(FailoverStop::DeadlineExceeded)));
        assert_eq!(out.attempts.len(), 1);
    }

    #[test]
    fn cloud_intent_failover_spend_cap_stops_paid_fallbacks() {
        // All candidates are paid; the cap admits at most a few attempts.
        let cs: Vec<Candidate> = (0..6)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut b = budget();
        b.spend = SpendPolicy::PaidAuthorized {
            max_spend_micro: 120,
        };
        b.attempt_estimate_micros = 50;
        let mut script = Script(ok_at(&[]));
        let out = run(
            &intent(),
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            b,
            &mut script,
        );
        assert!(out.output.is_none());
        assert!(ledger.account_exposure("") <= 120);
    }

    #[test]
    fn cloud_intent_failover_pin_denies_silent_fallback() {
        // A denied-fallback pin that fails returns nothing — never a quiet
        // substitution.
        let cs: Vec<Candidate> = (0..3)
            .map(|i| cand(&format!("sk-{i}"), &format!("m{i}")))
            .collect();
        let mut elig = EligibilityStore::new();
        let quota = QuotaInventory::new();
        let mut lock = LockoutTracker::new(Default::default(), crate::cloud_lockout::now_unix);
        let ledger = BudgetLedger::new();
        let mut i = intent();
        i.pinned_model = Some("m0".into());
        i.pin_fallback = susi_gawd_agents::cloud_intent::PinFallback::Deny;
        let mut script = Script(ok_at(&[]));
        let out = run(
            &i,
            &cs,
            &mut Stores {
                eligibility: &mut elig,
                quota: &quota,
                lockouts: &mut lock,
                ledger: &ledger,
            },
            budget(),
            &mut script,
        );
        assert!(out.output.is_none());
        assert_eq!(out.attempts.len(), 1);
    }
}
