//! Free-only and paid budget enforcement across parallel model attempts.
//!
//! Dispatch never spends silently. A [`BudgetLedger`] is the single atomic
//! point where workers, fallback legs, and probes reserve an *estimate*
//! before attempting, then reconcile the *actual* usage afterwards. The
//! ledger is keyed by **account** first — two keys on one billing account
//! share the same budget, so rotation cannot bypass a limit.
//!
//! Policies are explicit:
//! - [`SpendPolicy::FreeOnly`] — paid attempts are denied; free-quota
//!   exhaustion surfaces `FreeExhausted` and never silently incurs a charge
//!   or triggers a top-up.
//! - [`SpendPolicy::PaidAuthorized`] — paid attempts allowed up to
//!   `max_spend_micro` (millionths of the account's currency unit).
//! - [`SpendPolicy::AskFirst`] — a paid fallback returns
//!   [`Denial::NeedsConsent`] so the caller can surface the existing consent
//!   flow instead of charging silently.
//!
//! Prices may be unknown: a paid attempt with no price uses the caller's
//! bounded worst-case estimate; committed usage with no reported figure
//! reconciles as the reserved estimate (conservative — never zero).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Millionths of a currency unit (USD-equivalent micros) — integer math only,
/// no floating point drift across reconciliations.
pub type Micros = u64;

/// The account's declared spend policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendPolicy {
    /// Only candidates with no cost may be dispatched.
    FreeOnly,
    /// Paid attempts allowed up to this total spend (micros).
    PaidAuthorized { max_spend_micro: Micros },
    /// Paid allowed only after explicit consent — the caller surfaces an
    /// authorization prompt; the ledger denies until it arrives.
    AskFirst { max_spend_micro: Micros },
}

/// Why a reservation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// Free tier is spent and paid spend is not authorized under the policy.
    FreeExhausted,
    /// Reservation would push committed+held spend past the cap.
    OverBudget {
        held_plus_committed: Micros,
        requested: Micros,
        cap: Micros,
    },
    /// A paid attempt under `AskFirst` — caller must obtain consent.
    NeedsConsent { estimate: Micros },
    /// Unknown price under `AskFirst` — consent must name a bound.
    NeedsPriceConsent { worst_case: Micros },
}

impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Denial::FreeExhausted => {
                write!(f, "free quota exhausted and paid spend is not authorized")
            }
            Denial::OverBudget {
                held_plus_committed,
                requested,
                cap,
            } => write!(
                f,
                "spend cap {cap} would be exceeded: {held_plus_committed} held/committed + {requested} requested"
            ),
            Denial::NeedsConsent { estimate } => {
                write!(f, "paid attempt needs consent (estimate {estimate} micros)")
            }
            Denial::NeedsPriceConsent { worst_case } => write!(
                f,
                "unknown price needs consent bounded at {worst_case} micros"
            ),
        }
    }
}

/// A held reservation — released by `commit` (reconcile actual) or
/// `release` (attempt never ran / produced no usage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation(pub u64);

#[derive(Debug, Clone)]
struct Held {
    /// Billing account — the shared scope keys rotate under.
    account: String,
    estimate: Micros,
}

/// Everything needed to hold spend for one attempt.
#[derive(Debug, Clone, Copy)]
pub struct ReserveRequest<'a> {
    /// Billing account — sibling keys share its budget.
    pub account: &'a str,
    /// Opaque credential fingerprint (never raw key material).
    pub credential_fp: &'a str,
    /// Model the attempt targets.
    pub model: &'a str,
    /// Declared spend policy for this dispatch.
    pub policy: SpendPolicy,
    /// Whether the candidate's price is zero/unknown-free.
    pub is_free_candidate: bool,
    /// Free-quota view from `cloud_quota` evidence.
    pub free_headroom: FreeHeadroom,
    /// Estimated cost in micros; 0 = price unknown.
    pub estimate: Micros,
}

/// Free-quota view handed in by the caller (from `cloud_quota`) — the ledger
/// does not inspect headers itself, it only consumes the boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeHeadroom {
    /// Provider evidence says free quota remains.
    Available,
    /// Provider evidence says free quota is spent (or reset unknown past).
    Exhausted,
    /// No evidence — treated as available for *free* candidates (a free
    /// call cannot spend money), but a paid attempt still needs its policy.
    Unknown,
}

/// Atomic budget ledger: one mutex guards held reservations and committed
/// spend per account. Lock ordering is trivial (one lock); critical sections
/// are O(accounts) bookkeeping — no I/O, no provider calls inside.
#[derive(Debug, Default)]
pub struct BudgetLedger {
    inner: Mutex<Ledger>,
}

#[derive(Debug, Default)]
struct Ledger {
    /// Committed (actual or reconciled) spend per account.
    committed: BTreeMap<String, Micros>,
    /// Held reservations by id.
    held: BTreeMap<u64, Held>,
}

impl BudgetLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Ledger> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Total spend currently exposed for an account (committed + held).
    /// Both counts share the account key — sibling keys cannot bypass it.
    #[must_use]
    pub fn account_exposure(&self, account: &str) -> Micros {
        let g = self.lock();
        let held: Micros = g
            .held
            .values()
            .filter(|h| h.account == account)
            .map(|h| h.estimate)
            .sum();
        g.committed.get(account).copied().unwrap_or(0) + held
    }

    /// Reserve `estimate` micros for one attempt under `policy`.
    ///
    /// `cost_per_mtok = None` marks a free candidate: with
    /// `FreeHeadroom::Available`/`Unknown` it reserves nothing (free calls
    /// cannot spend); with `Exhausted` under a paid policy it may still be
    /// dispatched but is billed as a paid attempt (the provider may charge
    /// once free quota is gone — conservative).
    pub fn reserve(&self, req: ReserveRequest<'_>) -> Result<Reservation, Denial> {
        let mut g = self.lock();
        let committed = g.committed.get(req.account).copied().unwrap_or(0);
        let held: Micros = g
            .held
            .values()
            .filter(|h| h.account == req.account)
            .map(|h| h.estimate)
            .sum();
        let exposure = committed.saturating_add(held);

        // Free candidate with headroom: zero-cost hold, any policy.
        if req.is_free_candidate && req.free_headroom != FreeHeadroom::Exhausted {
            return Ok(new_hold(&mut g, req.account, 0));
        }
        // From here the attempt may incur spend.
        match req.policy {
            SpendPolicy::FreeOnly => Err(Denial::FreeExhausted),
            SpendPolicy::AskFirst { .. } => {
                if req.estimate == 0 {
                    Err(Denial::NeedsPriceConsent {
                        worst_case: u64::MAX,
                    })
                } else {
                    Err(Denial::NeedsConsent {
                        estimate: req.estimate,
                    })
                }
            }
            SpendPolicy::PaidAuthorized { max_spend_micro } => {
                if req.estimate == 0 {
                    // Unknown price on a paid path: bound by remaining cap.
                    if exposure < max_spend_micro {
                        let bound = max_spend_micro - exposure;
                        return Ok(new_hold(&mut g, req.account, bound));
                    }
                    return Err(Denial::OverBudget {
                        held_plus_committed: exposure,
                        requested: 0,
                        cap: max_spend_micro,
                    });
                }
                if exposure.saturating_add(req.estimate) > max_spend_micro {
                    return Err(Denial::OverBudget {
                        held_plus_committed: exposure,
                        requested: req.estimate,
                        cap: max_spend_micro,
                    });
                }
                Ok(new_hold(&mut g, req.account, req.estimate))
            }
        }
    }

    /// Reserve budget for a selected candidate immediately before dispatch.
    ///
    /// Keeping this operation on the injected ledger lets parallel callers
    /// share one atomic account budget while tests and failover runs avoid
    /// hidden process-global state.
    pub fn reserve_dispatch(
        &self,
        candidate: &crate::cloud_intent::Candidate,
        free_headroom: FreeHeadroom,
        estimate: Micros,
        policy: SpendPolicy,
    ) -> Result<Reservation, Denial> {
        let credential_fp =
            susi_vendor_models::cloud_eligibility::credential_fingerprint(&candidate.api_key);
        self.reserve(ReserveRequest {
            account: candidate.account.as_deref().unwrap_or(""),
            credential_fp: &credential_fp,
            model: &candidate.model,
            policy,
            is_free_candidate: candidate.cost_per_mtok.unwrap_or(0.0) == 0.0,
            free_headroom,
            estimate,
        })
    }

    /// Authorize a `NeedsConsent`/`NeedsPriceConsent` path after the user
    /// consented — equivalent to `PaidAuthorized` for this account now.
    /// Consent is a deliberate caller upgrade, not a ledger decision.
    #[must_use]
    pub fn consent_granted(policy: SpendPolicy) -> SpendPolicy {
        match policy {
            SpendPolicy::AskFirst { max_spend_micro } => {
                SpendPolicy::PaidAuthorized { max_spend_micro }
            }
            SpendPolicy::FreeOnly | SpendPolicy::PaidAuthorized { .. } => policy,
        }
    }

    /// Reconcile a held reservation with actual usage. `actual = None`
    /// (usage unreported) charges the full estimate — conservative, never
    /// zero. `actual > estimate` charges actual (retry/overrun honesty);
    /// `actual < estimate` refunds the difference.
    pub fn commit(&self, res: Reservation, actual: Option<Micros>) {
        let mut g = self.lock();
        if let Some(h) = g.held.remove(&res.0) {
            let charge = actual.unwrap_or(h.estimate);
            *g.committed.entry(h.account).or_insert(0) += charge;
        }
    }

    /// Release a hold whose attempt never produced usage (dispatch rejected
    /// before the call, transport failure pre-send).
    pub fn release(&self, res: Reservation) {
        self.lock().held.remove(&res.0);
    }
}

fn new_hold(g: &mut Ledger, account: &str, estimate: Micros) -> Reservation {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    g.held.insert(
        id,
        Held {
            account: account.to_string(),
            estimate,
        },
    );
    Reservation(id)
}

/// One global ledger for the process — workers share it so concurrent
/// attempts reserve against the same numbers.
static GLOBAL: OnceLock<BudgetLedger> = OnceLock::new();

/// The process-wide ledger. Tests construct their own `BudgetLedger`.
#[must_use]
pub fn global() -> &'static BudgetLedger {
    GLOBAL.get_or_init(BudgetLedger::new)
}

/// Reserve budget for a selected candidate right before dispatch.
/// `is_free`/`free_headroom`/`estimate` come from the caller's evidence
/// (quota inventory + model pricing); the ledger enforces the policy
/// atomically so parallel workers cannot overrun a cap together.
pub fn reserve_dispatch(
    candidate: &crate::cloud_intent::Candidate,
    free_headroom: FreeHeadroom,
    estimate: Micros,
    policy: SpendPolicy,
) -> Result<Reservation, Denial> {
    global().reserve_dispatch(candidate, free_headroom, estimate, policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const POLICY: SpendPolicy = SpendPolicy::PaidAuthorized {
        max_spend_micro: 1_000,
    };

    fn req<'a>(
        account: &'a str,
        policy: SpendPolicy,
        free: bool,
        headroom: FreeHeadroom,
        estimate: Micros,
    ) -> ReserveRequest<'a> {
        ReserveRequest {
            account,
            credential_fp: "fp-test",
            model: "m",
            policy,
            is_free_candidate: free,
            free_headroom: headroom,
            estimate,
        }
    }

    #[test]
    fn cloud_free_paid_budget_shared_keys_share_the_account_cap() {
        // Two rotating keys on one account must not double the budget.
        let l = BudgetLedger::new();
        let a = l
            .reserve(req("acct-1", POLICY, false, FreeHeadroom::Exhausted, 600))
            .unwrap();
        let b = l
            .reserve(req("acct-1", POLICY, false, FreeHeadroom::Exhausted, 600))
            .unwrap_err();
        assert!(matches!(b, Denial::OverBudget { .. }));
        l.release(a);
        // After release the second key can reserve — rotation is fine, the
        // account cap is what held.
        assert!(l
            .reserve(req("acct-1", POLICY, false, FreeHeadroom::Exhausted, 600),)
            .is_ok());
    }

    #[test]
    fn cloud_free_paid_budget_concurrent_workers_cannot_overrun() {
        // Ten workers race for a 1000-micro cap at 150 each: exactly six
        // holds (900) fit — never seven.
        let l = Arc::new(BudgetLedger::new());
        let mut wins = Vec::new();
        let mut handles = Vec::new();
        for _i in 0..10 {
            let l = Arc::clone(&l);
            handles.push(std::thread::spawn(move || {
                l.reserve(req(
                    "acct-shared",
                    POLICY,
                    false,
                    FreeHeadroom::Exhausted,
                    150,
                ))
                .ok()
            }));
        }
        for h in handles {
            if let Some(r) = h.join().unwrap() {
                wins.push(r);
            }
        }
        assert_eq!(wins.len(), 6);
        assert_eq!(l.account_exposure("acct-shared"), 900);
    }

    #[test]
    fn cloud_free_paid_budget_free_only_never_spends() {
        let l = BudgetLedger::new();
        // Free candidate with headroom: zero-cost hold under any policy.
        assert!(l
            .reserve(req(
                "acct",
                SpendPolicy::FreeOnly,
                true,
                FreeHeadroom::Available,
                0
            ),)
            .is_ok());
        // Free exhausted → the same candidate may be billed by the provider;
        // FreeOnly refuses rather than silently incurring a charge.
        assert_eq!(
            l.reserve(req(
                "acct",
                SpendPolicy::FreeOnly,
                true,
                FreeHeadroom::Exhausted,
                0
            ),)
                .unwrap_err(),
            Denial::FreeExhausted
        );
        // And an explicitly paid candidate is refused outright.
        assert_eq!(
            l.reserve(req(
                "acct",
                SpendPolicy::FreeOnly,
                false,
                FreeHeadroom::Available,
                10
            ),)
                .unwrap_err(),
            Denial::FreeExhausted
        );
    }

    #[test]
    fn cloud_free_paid_budget_ask_first_surfaces_consent() {
        let l = BudgetLedger::new();
        let ask = SpendPolicy::AskFirst {
            max_spend_micro: 500,
        };
        assert!(matches!(
            l.reserve(req("acct", ask, false, FreeHeadroom::Exhausted, 200),)
                .unwrap_err(),
            Denial::NeedsConsent { estimate: 200 }
        ));
        // Unknown price: consent must name a bound — never a blank check.
        assert!(matches!(
            l.reserve(req("acct", ask, false, FreeHeadroom::Exhausted, 0),)
                .unwrap_err(),
            Denial::NeedsPriceConsent { .. }
        ));
        // After consent the same reserve succeeds.
        let authorized = BudgetLedger::consent_granted(ask);
        assert!(l
            .reserve(req("acct", authorized, false, FreeHeadroom::Exhausted, 200),)
            .is_ok());
    }

    #[test]
    fn cloud_free_paid_budget_missing_usage_charges_the_estimate() {
        let l = BudgetLedger::new();
        let r = l
            .reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 300))
            .unwrap();
        // Provider reports nothing — the estimate stands, not zero.
        l.commit(r, None);
        assert_eq!(l.account_exposure("acct"), 300);
        // Reported actual replaces the estimate.
        let r2 = l
            .reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 300))
            .unwrap();
        l.commit(r2, Some(120));
        assert_eq!(l.account_exposure("acct"), 420);
    }

    #[test]
    fn cloud_free_paid_budget_retries_never_double_charge() {
        let l = BudgetLedger::new();
        // Attempt A reserves, fails pre-send → released; B reserves the same
        // scope and commits. One charge, not two.
        let a = l
            .reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 400))
            .unwrap();
        l.release(a);
        let b = l
            .reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 400))
            .unwrap();
        l.commit(b, Some(400));
        assert_eq!(l.account_exposure("acct"), 400);
        // A stale reservation id double-committing is a no-op.
        l.commit(b, Some(400));
        assert_eq!(l.account_exposure("acct"), 400);
    }

    #[test]
    fn cloud_free_paid_budget_unknown_price_reserves_the_remaining_cap() {
        let l = BudgetLedger::new();
        // Unknown-price paid attempt is bounded by what's left, and it
        // consumes the whole remainder so nothing else sneaks in behind it.
        let r = l
            .reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 0))
            .unwrap();
        assert!(matches!(
            l.reserve(req("acct", POLICY, false, FreeHeadroom::Exhausted, 1),)
                .unwrap_err(),
            Denial::OverBudget { .. }
        ));
        l.release(r);
    }

    #[test]
    fn cloud_budget() {
        let ledger = BudgetLedger::new();
        let policy = SpendPolicy::PaidAuthorized {
            max_spend_micro: 10_000,
        };

        let reservation = ledger
            .reserve(req(
                "account",
                policy,
                false,
                FreeHeadroom::Exhausted,
                5_000,
            ))
            .unwrap();
        ledger.commit(reservation, Some(4_000));
        assert_eq!(ledger.account_exposure("account"), 4_000);

        let reservation2 = ledger
            .reserve(req(
                "account",
                policy,
                false,
                FreeHeadroom::Exhausted,
                6_000,
            ))
            .unwrap();
        ledger.commit(reservation2, Some(6_000));
        assert_eq!(ledger.account_exposure("account"), 10_000);

        assert!(matches!(
            ledger
                .reserve(req("account", policy, false, FreeHeadroom::Exhausted, 1))
                .unwrap_err(),
            Denial::OverBudget { .. }
        ));
    }
}
