//! Mid-mission brain failover and restore (T-CODEX-26 / VC-201-012).
//!
//! During a multi-step mission the strongest working cloud model is the
//! brain. When it stops working — exhausted credit/quota, lockout, 5xx,
//! access error — the supervisor promptly fails the step over to the
//! strongest remaining working target through the production failover
//! path. Mission context (receipts, transitions) is preserved across
//! switches and uncertain side effects are never replayed: the failover
//! runner's ambiguity tracking carries forward as mission `dirty`.
//!
//! Restore: the original stronger brain is restored **only at step
//! boundaries** and only when fresh `Usable` evidence recorded *after*
//! the failure proves recovery, plus a dwell period to prevent
//! oscillation. A restore attempt is a single pinned dispatch — if it
//! fails, failover proceeds to other candidates in the same step.
//!
//! Every transition is recorded with a truthful reason; a successful
//! selection or probe is never reported as intent success — only a
//! completed step is.

use susi_gawd_agents::cloud_intent::{select, Candidate, IntentConstraints, Ranked, Selection};
use susi_vendor_models::cloud_eligibility::{EligibilityKind, Subject};

use crate::cloud_failover::{run_selection, FailoverBudget, Runner, Stores};

/// Why the brain changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionReason {
    /// First brain of the mission.
    Initial,
    /// Current brain failed mid-step or between steps.
    BrainFailed {
        /// Redacted failure kind (http_402, lockout, timeout, ...).
        kind: String,
    },
    /// Fresh Usable evidence + dwell elapsed — restored at a boundary.
    Recovered,
}

/// One brain transition in the mission's audit trail.
#[derive(Debug, Clone)]
pub struct BrainTransition {
    /// When it happened (ms, injected clock domain).
    pub at_ms: u64,
    /// Previous brain (opaque id), if any.
    pub from: Option<String>,
    /// New brain (opaque id), if any.
    pub to: Option<String>,
    /// Why.
    pub reason: TransitionReason,
}

/// Durable mission state carried across steps and brain switches.
#[derive(Debug, Default)]
pub struct BrainMission {
    /// Opaque id of the strongest brain chosen at mission start.
    pub preferred: Option<String>,
    /// Index into `candidates` of `preferred`.
    pub preferred_index: Option<usize>,
    /// Opaque id of the brain currently serving.
    pub current: Option<String>,
    /// When we fell back off `preferred` (ms).
    pub fell_back_at_ms: Option<u64>,
    /// Ordered receipts from every dispatch attempt — the audit trail.
    pub receipts: Vec<String>,
    /// Transition log.
    pub transitions: Vec<BrainTransition>,
    /// A prior ambiguous attempt may have external side effects — the
    /// mission is dirty and its output is not proven clean.
    pub dirty: bool,
    /// Restore attempts used — bounded by `SupervisorPolicy::max_restores`.
    pub restores: u32,
}

impl BrainMission {
    fn transition(&mut self, at_ms: u64, to: Option<String>, reason: TransitionReason) {
        self.transitions.push(BrainTransition {
            at_ms,
            from: self.current.clone(),
            to: to.clone(),
            reason,
        });
        self.current = to;
    }
}

/// Supervisor policy.
#[derive(Debug, Clone, Copy)]
pub struct SupervisorPolicy {
    /// Minimum ms on a fallback brain before the preferred brain may be
    /// restored — hysteresis against oscillation.
    pub min_dwell_ms: u64,
    /// Maximum restore attempts per mission — a hard bound on oscillation.
    pub max_restores: u32,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            // One minute on a fallback before a restore probe is legal.
            min_dwell_ms: 60_000,
            max_restores: 3,
        }
    }
}

/// One step's request: intent constraints, dispatch budget, policy.
#[derive(Debug, Clone, Copy)]
pub struct StepSpec<'a> {
    /// What the step needs from a model.
    pub intent: &'a IntentConstraints,
    /// Attempt/deadline/spend bound.
    pub budget: FailoverBudget,
    /// Supervisor policy.
    pub policy: SupervisorPolicy,
}

/// Result of one mission step.
#[derive(Debug)]
pub enum StepOutcome {
    /// The step completed on `brain` with `output`.
    Completed { brain: String, output: String },
    /// No working brain could run the step — honest block, not success.
    Blocked { reason: String },
}

fn subject_of(c: &Candidate) -> Subject<'_> {
    Subject {
        provider: &c.provider,
        api_key: &c.api_key,
        account: c.account.as_deref(),
        region: c.region.as_deref(),
        model: &c.model,
    }
}

/// Restore check: preferred has fresh `Usable` evidence recorded after the
/// fallback, and the dwell period has elapsed. `restores` counts against
/// `policy.max_restores`.
fn preferred_recovered(
    mission: &BrainMission,
    stores: &Stores<'_>,
    candidates: &[Candidate],
    policy: &SupervisorPolicy,
    now_ms: u64,
) -> bool {
    let (Some(pref), Some(pi)) = (&mission.preferred, mission.preferred_index) else {
        return false;
    };
    // Already on preferred — nothing to restore.
    if mission.current.as_deref() == Some(pref.as_str()) {
        return false;
    }
    if mission.restores >= policy.max_restores {
        return false;
    }
    let Some(fell_at) = mission.fell_back_at_ms else {
        return false;
    };
    if now_ms.saturating_sub(fell_at) < policy.min_dwell_ms {
        return false;
    }
    let Some(c) = candidates.get(pi) else {
        return false;
    };
    let v = stores.eligibility.resolve(subject_of(c), now_ms / 1000);
    // Fresh recovery evidence: a Usable verdict observed *after* we fell
    // back — not the stale pre-failure record.
    v.kind == EligibilityKind::Usable && v.observed_unix.saturating_mul(1000) > fell_at
}

/// Record which previous brain failed, with its redacted failure kind,
/// from this step's attempt log.
fn failure_kind(result: &crate::cloud_failover::FailoverResult, current: &str) -> String {
    result
        .attempts
        .iter()
        .find(|a| a.candidate == current)
        .map(|a| format!("{:?}", a.outcome))
        .unwrap_or_else(|| "no_longer_dispatchable".to_string())
}

/// Run one mission step. `runner` executes each dispatch attempt;
/// `spec.budget` bounds attempts/deadline/spend for the step.
#[must_use]
pub fn run_step<R: Runner>(
    mission: &mut BrainMission,
    stores: &mut Stores<'_>,
    candidates: &[Candidate],
    spec: &StepSpec<'_>,
    runner: &mut R,
) -> StepOutcome {
    let budget = &spec.budget;
    let intent = spec.intent;
    let policy = &spec.policy;
    let now_ms = budget.now_ms;

    // --- Restore at the step boundary (hysteresis) ---
    if preferred_recovered(mission, stores, candidates, policy, now_ms) {
        mission.restores += 1;
        let pref = mission.preferred.clone();
        mission.transition(now_ms, pref, TransitionReason::Recovered);
        mission.fell_back_at_ms = None;
        // Pinned single dispatch to the restored brain — a failure falls
        // through to the normal failover run below.
        if let Some(pi) = mission.preferred_index {
            let pinned = Selection {
                ranked: vec![Ranked {
                    index: pi,
                    candidate: mission.preferred.clone().unwrap_or_default(),
                    discovery: false,
                    pinned: false,
                    score: 0.0,
                }],
                rejected: Vec::new(),
            };
            let res = run_selection(&pinned, candidates, stores, *budget, runner);
            mission.dirty |= res.prior_ambiguous;
            for a in &res.attempts {
                mission
                    .receipts
                    .push(format!("attempt {} -> {}", a.candidate, a.outcome));
            }
            if let Some(output) = res.output {
                return StepOutcome::Completed {
                    brain: mission.preferred.clone().unwrap_or_default(),
                    output,
                };
            }
            // Restore dispatch failed — evidence already recorded by
            // run_selection; proceed to normal failover this step.
        }
    }

    // --- Normal step through production failover ---
    // Hysteresis: while on a fallback brain, suppress the preferred brain
    // from this step's ranking — it may only regain the lead through the
    // gated restore path above, never by slipping back through selection.
    let mut sel = select(
        intent,
        candidates,
        stores.eligibility,
        stores.quota,
        budget.now_ms / 1000,
    );
    if mission.fell_back_at_ms.is_some() && mission.current != mission.preferred {
        sel.ranked
            .retain(|r| Some(r.index) != mission.preferred_index);
    }
    let res = run_selection(&sel, candidates, stores, *budget, runner);
    mission.dirty |= res.prior_ambiguous;
    for a in &res.attempts {
        mission
            .receipts
            .push(format!("attempt {} -> {}", a.candidate, a.outcome));
    }
    // Capture the outgoing brain's failure kind before consuming `res`.
    let outgoing_failure = mission.current.as_ref().map(|cur| failure_kind(&res, cur));
    match (res.winner, res.output) {
        (Some(wi), Some(output)) => {
            let winner_id = candidates[wi].opaque_id();
            match &mission.current {
                None => {
                    // Mission start — record the preferred brain too.
                    if mission.preferred.is_none() {
                        mission.preferred = Some(winner_id.clone());
                        mission.preferred_index = Some(wi);
                    }
                    mission.transition(now_ms, Some(winner_id.clone()), TransitionReason::Initial);
                }
                Some(cur) if cur != &winner_id => {
                    // The old brain lost this step — record its failure.
                    let kind = outgoing_failure.unwrap_or_default();
                    if Some(cur) == mission.preferred.as_ref() {
                        mission.fell_back_at_ms = Some(now_ms);
                    }
                    mission.transition(
                        now_ms,
                        Some(winner_id.clone()),
                        TransitionReason::BrainFailed { kind },
                    );
                }
                Some(_) => {}
            }
            StepOutcome::Completed {
                brain: winner_id,
                output,
            }
        }
        _ => {
            let reason = res
                .stop
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "unknown".to_string());
            StepOutcome::Blocked { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_lockout::LockoutTracker;
    use std::collections::BTreeMap;
    use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
    use susi_vendor_models::cloud_eligibility::{EligibilityStore, InferenceResult};
    use susi_vendor_models::cloud_quota::QuotaInventory;

    const T0: u64 = 1_700_000_000;
    const T0_MS: u64 = T0 * 1000;

    use crate::cloud_failover::AttemptOutcome;

    fn cand(key: &str, model: &str) -> Candidate {
        Candidate {
            provider: "acme".into(),
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

    /// Scripted runner: per-candidate-index behavior by step.
    struct Script {
        /// (candidate_index, call_number) -> outcome
        fails: BTreeMap<usize, Vec<AttemptOutcome>>,
        calls: Vec<usize>,
    }
    impl Script {
        fn ok() -> Self {
            Self {
                fails: BTreeMap::new(),
                calls: Vec::new(),
            }
        }
    }
    impl Runner for Script {
        fn attempt(&mut self, index: usize, _dl: u64) -> AttemptOutcome {
            self.calls.push(index);
            self.fails
                .get_mut(&index)
                .and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.remove(0))
                    }
                })
                .unwrap_or(AttemptOutcome::Success(format!("out-{index}")))
        }
    }

    struct Fx {
        elig: EligibilityStore,
        quota: QuotaInventory,
        lock: LockoutTracker,
        ledger: BudgetLedger,
        dir: std::path::PathBuf,
        ledger_dir: std::path::PathBuf,
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

    fn fx(tag: &str) -> Fx {
        let dir = std::env::temp_dir().join(format!("bs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Fx {
            elig: EligibilityStore::new(),
            quota: QuotaInventory::new(),
            lock: LockoutTracker::default(),
            ledger: BudgetLedger::new(),
            dir: dir.clone(),
            ledger_dir: std::env::temp_dir().join(format!("bsl-{tag}-{}", std::process::id())),
        }
    }

    fn intent() -> IntentConstraints {
        IntentConstraints {
            task_class: "reasoning".into(),
            ..Default::default()
        }
    }

    fn spec<'a>(intent: &'a IntentConstraints, now_ms: u64) -> StepSpec<'a> {
        StepSpec {
            intent,
            budget: FailoverBudget {
                max_attempts: 4,
                deadline_ms: None,
                spend: SpendPolicy::PaidAuthorized {
                    max_spend_micro: 1_000_000,
                },
                // A real per-attempt estimate — production never dispatches
                // a paid path with an unbounded unknown-price hold.
                attempt_estimate_micros: 10,
                now_ms,
            },
            policy: SupervisorPolicy {
                min_dwell_ms: 1_000,
                max_restores: 3,
            },
        }
    }

    /// Mid-step failure: the preferred brain's attempt fails inside the
    /// step and the same step completes on the fallback — transition and
    /// receipts are recorded.
    #[test]
    fn powerful_cloud_brain_recovery_midstep_failover() {
        let mut f = fx("midstep");
        let mut b = cand("sk-b", "m-b");
        b.provider = "beta".into();
        let cs = vec![cand("sk-a", "m-a"), b];
        for c in &cs {
            f.elig
                .record_inference(subj(c), &InferenceResult::Success, T0);
        }
        let mut m = BrainMission::default();
        // Step 1: brain A works.
        let mut s = Script::ok();
        match run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS),
            &mut s,
        ) {
            StepOutcome::Completed { brain, .. } => assert!(brain.contains("m-a")),
            _ => panic!("step1"),
        }
        assert_eq!(m.transitions.len(), 1);
        // Step 2: brain A fails with a 503 mid-step — same step finishes on B.
        f.elig.record_inference(
            subj(&cs[0]),
            &InferenceResult::Failed {
                status: Some(503),
                body_snippet: "down".into(),
                retry_after_secs: None,
            },
            T0 + 1,
        );
        match run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 500),
            &mut s,
        ) {
            StepOutcome::Completed { brain, .. } => assert!(brain.contains("m-b")),
            _ => panic!("step2"),
        }
        assert!(matches!(
            m.transitions[1].reason,
            TransitionReason::BrainFailed { .. }
        ));
        assert!(!m.receipts.is_empty());
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&f.ledger_dir);
    }

    /// Stronger brain is restored at a step boundary once fresh Usable
    /// evidence exists AND dwell elapsed — not before.
    #[test]
    fn powerful_cloud_brain_recovery_restore_needs_fresh_evidence_and_dwell() {
        let mut f = fx("restore");
        let mut b = cand("sk-b", "m-b");
        b.provider = "beta".into();
        let cs = vec![cand("sk-a", "m-a"), b];
        for c in &cs {
            f.elig
                .record_inference(subj(c), &InferenceResult::Success, T0);
        }
        let mut m = BrainMission::default();
        let mut s = Script::ok();
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS),
            &mut s,
        );
        // Brain A dies → fallback to B at t+500.
        f.elig.record_inference(
            subj(&cs[0]),
            &InferenceResult::Failed {
                status: Some(429),
                body_snippet: "rate limited".into(),
                retry_after_secs: Some(60),
            },
            T0 + 1,
        );
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 500),
            &mut s,
        );
        assert!(m.current.as_deref().unwrap().contains("m-b"));
        // Recovery evidence arrives (probe success at T0+2) but dwell not
        // elapsed at t+800 → stays on B.
        f.elig
            .record_inference(subj(&cs[0]), &InferenceResult::Success, T0 + 2);
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 800),
            &mut s,
        );
        assert!(m.current.as_deref().unwrap().contains("m-b"));
        // Dwell elapsed at t+1600 → restore to A via pinned dispatch.
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 1600),
            &mut s,
        );
        assert!(m.current.as_deref().unwrap().contains("m-a"));
        assert!(m
            .transitions
            .iter()
            .any(|t| t.reason == TransitionReason::Recovered));
        assert_eq!(m.restores, 1);
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&f.ledger_dir);
    }

    /// All clouds blocked → honest Blocked, no fake success.
    #[test]
    fn powerful_cloud_brain_recovery_all_blocked_is_honest() {
        let mut f = fx("allblocked");
        let cs = vec![cand("sk-a", "m-a")];
        f.elig.record_inference(
            subj(&cs[0]),
            &InferenceResult::Failed {
                status: Some(402),
                body_snippet: "no credit".into(),
                retry_after_secs: None,
            },
            T0,
        );
        let mut m = BrainMission::default();
        let mut s = Script::ok();
        match run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS),
            &mut s,
        ) {
            StepOutcome::Blocked { reason } => assert!(!reason.is_empty()),
            _ => panic!("expected Blocked"),
        }
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&f.ledger_dir);
    }

    /// Oscillation bound: restore attempts are capped by policy.
    #[test]
    fn powerful_cloud_brain_recovery_restore_is_bounded() {
        let mut f = fx("osc");
        let mut b = cand("sk-b", "m-b");
        b.provider = "beta".into();
        let cs = vec![cand("sk-a", "m-a"), b];
        for c in &cs {
            f.elig
                .record_inference(subj(c), &InferenceResult::Success, T0);
        }
        let mut m = BrainMission::default();
        let pol = SupervisorPolicy {
            min_dwell_ms: 0,
            max_restores: 1,
        };
        let mut s = Script::ok();
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &StepSpec {
                policy: pol,
                ..spec(&intent(), T0_MS)
            },
            &mut s,
        );
        // Fail A → fallback to B.
        f.elig.record_inference(
            subj(&cs[0]),
            &InferenceResult::Failed {
                status: Some(503),
                body_snippet: "x".into(),
                retry_after_secs: None,
            },
            T0 + 1,
        );
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &StepSpec {
                policy: pol,
                ..spec(&intent(), T0_MS + 100)
            },
            &mut s,
        );
        // Fresh evidence → restore 1 (cap reached).
        f.elig
            .record_inference(subj(&cs[0]), &InferenceResult::Success, T0 + 2);
        // The restore probe fails (scripted) → falls back to B again.
        s.fails.insert(
            0,
            vec![AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: Some(503),
                body_snippet: "x".into(),
                retry_after_secs: None,
            })],
        );
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &StepSpec {
                policy: pol,
                ..spec(&intent(), T0_MS + 200)
            },
            &mut s,
        );
        // New recovery evidence — but max_restores=1 forbids another.
        f.elig
            .record_inference(subj(&cs[0]), &InferenceResult::Success, T0 + 3);
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &StepSpec {
                policy: pol,
                ..spec(&intent(), T0_MS + 300)
            },
            &mut s,
        );
        assert_eq!(m.restores, 1);
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&f.ledger_dir);
    }

    /// A restored brain that fails its pinned dispatch falls back within
    /// the same step — mid-step failure after restore is still failover.
    #[test]
    fn powerful_cloud_brain_recovery_failed_restore_fails_over_same_step() {
        let mut f = fx("restfail");
        let mut b = cand("sk-b", "m-b");
        b.provider = "beta".into();
        let cs = vec![cand("sk-a", "m-a"), b];
        for c in &cs {
            f.elig
                .record_inference(subj(c), &InferenceResult::Success, T0);
        }
        let mut m = BrainMission::default();

        let mut s = Script::ok();
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS),
            &mut s,
        );
        f.elig.record_inference(
            subj(&cs[0]),
            &InferenceResult::Failed {
                status: Some(503),
                body_snippet: "x".into(),
                retry_after_secs: None,
            },
            T0 + 1,
        );
        let _ = run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 500),
            &mut s,
        );
        // Recovery evidence + dwell → restore pinned to A; A's dispatch
        // fails → same step still completes on B.
        f.elig
            .record_inference(subj(&cs[0]), &InferenceResult::Success, T0 + 2);
        s.fails.insert(
            0,
            vec![AttemptOutcome::PreDispatch(InferenceResult::Failed {
                status: Some(500),
                body_snippet: "still bad".into(),
                retry_after_secs: None,
            })],
        );
        match run_step(
            &mut m,
            &mut f.stores(),
            &cs,
            &spec(&intent(), T0_MS + 2000),
            &mut s,
        ) {
            StepOutcome::Completed { brain, .. } => assert!(brain.contains("m-b")),
            _ => panic!("expected completion on fallback"),
        }
        let _ = std::fs::remove_dir_all(&f.dir);
        let _ = std::fs::remove_dir_all(&f.ledger_dir);
    }
}
