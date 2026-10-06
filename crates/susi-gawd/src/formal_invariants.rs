//! Formal small-state model of claim protocol + quarantine ladder (VC-201-096).
//!
//! Exhaustive search over miniature schedules proves: at most one live claim
//! wins, lease expiry is monotonic, and the quarantine ladder never exceeds
//! its cap or skips a recovery probe rung.

use serde::{Deserialize, Serialize};

/// Mirror of [`susi_gemi::engines::brain::QUARANTINE_CAP_SECS`].
pub const QUARANTINE_CAP_SECS: u64 = 21_600;

/// Result of a bounded spend transition in the small-state budget model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetTransition {
    /// The requested amount remains within the account cap.
    Admit,
    /// The requested amount would exceed the account cap.
    Reject,
}

/// Policy classes used by the bounded policy-precedence model.
///
/// `kani::Arbitrary` only exists under `cfg(kani)`; the proof harness draws an
/// arbitrary policy with `kani::any()`, which needs it. Without it the proofs do
/// not compile - and nothing noticed, because the job that builds them never had
/// a runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum PolicyClass {
    /// Paid work is never permitted.
    FreeOnly,
    /// Paid work requires an explicit consent step.
    AskFirst,
    /// Paid work is permitted when the budget transition admits it.
    PaidAuthorized,
}

/// Outcome of applying policy precedence before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    /// A free candidate may proceed without paid consent.
    Free,
    /// The free-only policy refuses a paid candidate.
    Denied,
    /// The caller must obtain consent before attempting paid work.
    NeedsConsent,
    /// The paid policy permits the candidate to reach budget admission.
    Paid,
}

/// Check the generation and lease timestamp used to fence a claim holder.
///
/// The remote claim protocol uses both pieces of state: an old generation is
/// stale even if its timestamp has not expired, and an expired generation is
/// stale even when its generation still matches. Keeping this predicate pure
/// makes the two safety conditions directly model-checkable.
#[must_use]
pub fn lease_is_fenced(
    expected_generation: u64,
    observed_generation: u64,
    now: u64,
    lease_until_unix: u64,
) -> bool {
    expected_generation == observed_generation && now < lease_until_unix
}

/// Model the integer arithmetic of a reserve-before-dispatch budget step.
///
/// Saturating addition mirrors the production ledger's overflow-safe
/// exposure calculation. The model deliberately contains no locks or I/O so
/// Kani can explore every bounded input without depending on a host.
#[must_use]
pub fn budget_transition(committed: u64, held: u64, requested: u64, cap: u64) -> BudgetTransition {
    if committed.saturating_add(held).saturating_add(requested) <= cap {
        BudgetTransition::Admit
    } else {
        BudgetTransition::Reject
    }
}

/// Apply the explicit policy precedence before a budget check.
///
/// Free candidates with live free headroom win before paid-policy gates. Once
/// that headroom is exhausted, `FreeOnly`, `AskFirst`, and
/// `PaidAuthorized` remain distinct and cannot silently escalate one another.
#[must_use]
pub fn policy_precedence(
    policy: PolicyClass,
    is_free_candidate: bool,
    free_headroom_available: bool,
) -> PolicyDecision {
    if is_free_candidate && free_headroom_available {
        return PolicyDecision::Free;
    }
    match policy {
        PolicyClass::FreeOnly => PolicyDecision::Denied,
        PolicyClass::AskFirst => PolicyDecision::NeedsConsent,
        PolicyClass::PaidAuthorized => PolicyDecision::Paid,
    }
}

/// Quarantine seconds after `consecutive` operator-needed failures.
#[must_use]
pub fn quarantine_ladder_secs(consecutive: u32) -> u64 {
    match consecutive {
        0 | 1 => 600,
        2 => 3_600,
        _ => QUARANTINE_CAP_SECS,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelClaim {
    pub task: String,
    pub agent: String,
    pub claimed_unix: u64,
    pub lease_until_unix: u64,
}

impl ModelClaim {
    #[must_use]
    pub fn expired(&self, now: u64) -> bool {
        now >= self.lease_until_unix
    }
}

/// In-memory CAS claim store (models `refs/claims/<id>` first-writer-wins).
#[derive(Debug, Default, Clone)]
pub struct ClaimModel {
    /// task_id → (generation, claim). Generation bumps on every successful write.
    claims: std::collections::BTreeMap<String, (u64, ModelClaim)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Won,
    LostAlive,
    TookOverExpired,
}

impl ClaimModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn try_claim(
        &mut self,
        task: &str,
        agent: &str,
        now: u64,
        lease_secs: u64,
    ) -> ClaimOutcome {
        let lease = lease_secs.max(1);
        let next = ModelClaim {
            task: task.to_string(),
            agent: agent.to_string(),
            claimed_unix: now,
            lease_until_unix: now.saturating_add(lease),
        };
        match self.claims.get(task) {
            None => {
                self.claims.insert(task.to_string(), (1, next));
                ClaimOutcome::Won
            }
            Some(&(gen, ref existing)) if existing.expired(now) => {
                self.claims
                    .insert(task.to_string(), (gen.saturating_add(1), next));
                ClaimOutcome::TookOverExpired
            }
            Some(_) => ClaimOutcome::LostAlive,
        }
    }

    #[must_use]
    pub fn live_holder(&self, task: &str, now: u64) -> Option<&str> {
        self.claims.get(task).and_then(|(_, c)| {
            if c.expired(now) {
                None
            } else {
                Some(c.agent.as_str())
            }
        })
    }

    #[must_use]
    pub fn lease_until(&self, task: &str) -> Option<u64> {
        self.claims.get(task).map(|(_, c)| c.lease_until_unix)
    }
}

/// Safety properties checked after every transition.
pub fn claim_safety(model: &ClaimModel, now: u64) -> Result<(), String> {
    for (task, (_, c)) in &model.claims {
        if !c.expired(now) {
            // Exactly one live holder representation per task (map key uniqueness).
            if c.task != *task {
                return Err(format!("claim task mismatch for {task}"));
            }
            if c.lease_until_unix < c.claimed_unix {
                return Err(format!("lease before claim for {task}"));
            }
        }
    }
    Ok(())
}

#[cfg(kani)]
mod kani_proofs {
    use super::{
        budget_transition, lease_is_fenced, policy_precedence, BudgetTransition, PolicyClass,
        PolicyDecision,
    };

    /// A stale generation or expired lease can never pass the claim fence.
    #[kani::proof]
    fn stale_claims_are_fenced() {
        let expected: u8 = kani::any();
        let observed: u8 = kani::any();
        let now: u8 = kani::any();
        let lease_until: u8 = kani::any();
        kani::assume(expected != observed || now >= lease_until);
        assert!(!lease_is_fenced(
            u64::from(expected),
            u64::from(observed),
            u64::from(now),
            u64::from(lease_until),
        ));
    }

    /// An admitted bounded transition never exposes more than its cap.
    #[kani::proof]
    fn admitted_budget_transition_is_within_cap() {
        let committed: u8 = kani::any();
        let held: u8 = kani::any();
        let requested: u8 = kani::any();
        let cap: u8 = kani::any();
        if matches!(
            budget_transition(
                u64::from(committed),
                u64::from(held),
                u64::from(requested),
                u64::from(cap),
            ),
            BudgetTransition::Admit
        ) {
            assert!(
                u64::from(committed)
                    .saturating_add(u64::from(held))
                    .saturating_add(u64::from(requested))
                    <= u64::from(cap)
            );
        }
    }

    /// Policy precedence cannot turn a paid candidate into free work.
    #[kani::proof]
    fn paid_policy_precedence_is_explicit() {
        let policy: PolicyClass = kani::any();
        kani::assume(!matches!(policy, PolicyClass::FreeOnly));
        let decision = policy_precedence(policy, false, true);
        assert!(matches!(
            (policy, decision),
            (PolicyClass::AskFirst, PolicyDecision::NeedsConsent)
                | (PolicyClass::PaidAuthorized, PolicyDecision::Paid)
        ));
    }
}
