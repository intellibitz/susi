//! The mission budget.
//!
//! Spend is bounded *before* an attempt is dispatched, not discovered after the
//! bill arrives. An attempt is admitted only when the money already spent,
//! money reserved by in-flight attempts, and a conservative estimate of this
//! attempt all fit inside the mission's limit. The estimate is deliberately
//! pessimistic: the prompt is priced at the cache-miss rate (a cold turn is the
//! common case) and the output allowance is priced in full.
//!
//! Two consequences matter:
//!
//! - A mission cannot overshoot its limit by starting work it cannot pay for.
//!   Several tasks run concurrently, so the reservation is shared: two
//!   attempts cannot both be admitted against the same remaining dollar.
//! - Running out of budget stops the mission instead of becoming a retry. The
//!   scheduler reports the refusal as a [`crate::scheduler::AttemptOutcome::Skipped`]
//!   and leaves the task for a later mission or a top-up.

use crate::money::MicroUsd;
use crate::pricing::{ModelPrice, TokenUsage};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Characters per token used to estimate a prompt before it is sent.
///
/// Real tokenizers differ, which is why the estimate is treated as a floor and
/// the output allowance is priced in full.
const CHARS_PER_TOKEN: u64 = 4;

/// Output tokens reserved for one attempt when the caller does not say.
pub const DEFAULT_OUTPUT_RESERVE: u64 = 8_192;

/// A mission's spending limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Most the mission may spend, in micro-USD.
    pub limit: MicroUsd,
    /// Output tokens reserved per attempt.
    #[serde(default = "default_output_reserve")]
    pub output_reserve: u64,
}

const fn default_output_reserve() -> u64 {
    DEFAULT_OUTPUT_RESERVE
}

impl Budget {
    /// A budget with the default per-attempt output reserve.
    #[must_use]
    pub const fn new(limit: MicroUsd) -> Self {
        Self {
            limit,
            output_reserve: DEFAULT_OUTPUT_RESERVE,
        }
    }

    /// What the mission has spent so far, read from the ledger.
    #[must_use]
    pub fn spent(ledger_path: &Path, mission: &str) -> Spend {
        Self::spent_any(ledger_path, &[mission])
    }

    /// What the rows booked under any of `missions` have spent, each row
    /// counted once: one spender whose rows were booked under more than one
    /// name ([`crate::ingress::Account::spent`]).
    #[must_use]
    pub fn spent_any(ledger_path: &Path, missions: &[&str]) -> Spend {
        // A torn line must not zero the spend: the strict reader refused the
        // whole file on one, and the guard then saw nothing spent.
        let entries = crate::ledger::read_entries_reporting(ledger_path);
        let mut spend = Spend::default();
        for entry in entries
            .iter()
            .filter(|entry| missions.contains(&entry.mission.as_str()))
        {
            spend.total = spend
                .total
                .checked_add(entry.cost)
                .unwrap_or(MicroUsd::from_micros(u64::MAX));
            spend.calls = spend.calls.saturating_add(1);
        }
        spend
    }

    /// The pessimistic cost of one attempt.
    ///
    /// The prompt is priced at the miss rate and the whole output allowance at
    /// the output rate. A cache hit or a short answer only makes the real cost
    /// smaller, so an admission decided on this estimate is never too generous.
    #[must_use]
    pub fn estimate(&self, price: ModelPrice, prompt: &str) -> MicroUsd {
        estimate_tokens(price, prompt, self.output_reserve)
    }

    /// Whether one attempt at `estimate` fits, considering what is spent and
    /// what in-flight attempts have reserved.
    #[must_use]
    pub fn admits(&self, spent: Spend, reserved: MicroUsd, estimate: MicroUsd) -> bool {
        spent
            .total
            .checked_add(reserved)
            .and_then(|committed| committed.checked_add(estimate))
            .is_some_and(|total| total <= self.limit)
    }

    /// What is left after the money already spent.
    #[must_use]
    pub fn remaining(&self, spent: Spend) -> MicroUsd {
        self.limit
            .checked_sub(spent.total)
            .unwrap_or(MicroUsd::ZERO)
    }

    /// True when spend has crossed the warning line (four fifths of the limit).
    #[must_use]
    pub fn at_risk(&self, spent: Spend) -> bool {
        spent.total.micros().saturating_mul(5) >= self.limit.micros().saturating_mul(4)
    }
}

/// The pessimistic cost of one turn: the prompt at the miss rate and the whole
/// output allowance at the output rate.
#[must_use]
pub fn estimate_tokens(price: ModelPrice, prompt: &str, output_reserve: u64) -> MicroUsd {
    let prompt_bytes = u64::try_from(prompt.len()).unwrap_or(u64::MAX);
    let input_tokens = prompt_bytes.div_ceil(CHARS_PER_TOKEN);
    price.cost(TokenUsage::new(input_tokens, 0, output_reserve))
}

/// What a mission has spent so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Spend {
    /// Priced calls recorded in the ledger.
    pub calls: u64,
    /// Total spend in micro-USD.
    pub total: MicroUsd,
}

/// Why an attempt was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The attempt fits inside the budget.
    Allowed,
    /// The attempt would cross the limit; nothing was reserved.
    Exhausted {
        /// Already spent.
        spent: MicroUsd,
        /// This attempt's estimate.
        estimate: MicroUsd,
        /// The mission's limit.
        limit: MicroUsd,
    },
}

/// A shared budget for the attempts a mission runs concurrently.
///
/// The guard keeps reservations in memory. The ledger remains the authority on
/// what was actually charged; [`BudgetGuard::settle`] folds the ledger's number
/// in after every attempt, so a crash that loses a reservation cannot make the
/// next run believe money is still free.
#[derive(Debug)]
pub struct BudgetGuard {
    budget: Budget,
    state: Mutex<GuardState>,
}

#[derive(Debug, Default, Clone, Copy)]
struct GuardState {
    committed: MicroUsd,
    reserved: MicroUsd,
}

impl BudgetGuard {
    /// Build a guard that starts from the ledger's spend.
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            state: Mutex::new(GuardState::default()),
        }
    }

    /// The budget this guard enforces.
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// Try to reserve one attempt's estimated cost.
    ///
    /// `ledger_spent` is re-read by the caller so a charge written by the
    /// gateway is never lost behind a stale reservation.
    pub fn admit(&self, ledger_spent: Spend, estimate: MicroUsd) -> Admission {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.committed = state.committed.max(ledger_spent.total);
        let committed = state.committed;
        let next_reserved = match state.reserved.checked_add(estimate) {
            Some(reserved) => reserved,
            None => {
                return Admission::Exhausted {
                    spent: committed,
                    estimate,
                    limit: self.budget.limit,
                }
            }
        };
        let fits = committed
            .checked_add(next_reserved)
            .is_some_and(|total| total <= self.budget.limit);
        if !fits {
            return Admission::Exhausted {
                spent: committed,
                estimate,
                limit: self.budget.limit,
            };
        }
        state.reserved = next_reserved;
        Admission::Allowed
    }

    /// Release a reservation and fold in the ledger's post-attempt spend.
    pub fn settle(&self, ledger_spent: Spend, estimate: MicroUsd) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.committed = state.committed.max(ledger_spent.total);
        state.reserved = state
            .reserved
            .checked_sub(estimate)
            .unwrap_or(MicroUsd::ZERO);
    }

    /// Release an unsettled reservation without folding in new ledger spend.
    ///
    /// Called automatically by [`BudgetReservation`]'s `Drop` implementation when
    /// a worker panics or aborts before settling its reservation.
    pub fn release_unsettled(&self, estimate: MicroUsd) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.reserved = state
            .reserved
            .checked_sub(estimate)
            .unwrap_or(MicroUsd::ZERO);
    }

    /// Reserve one attempt's estimated cost with an RAII [`BudgetReservation`] guard.
    ///
    /// If the returned guard is dropped before [`BudgetReservation::settle`] is
    /// called, its `Drop` implementation automatically releases `estimate` from
    /// the guard's in-flight reservation pool.
    ///
    /// # Errors
    ///
    /// Returns [`Admission::Exhausted`] when the attempt would cross the budget limit.
    pub fn reserve(
        &self,
        ledger_spent: Spend,
        estimate: MicroUsd,
    ) -> Result<BudgetReservation<'_>, Admission> {
        match self.admit(ledger_spent, estimate) {
            Admission::Allowed => Ok(BudgetReservation {
                guard: self,
                estimate,
                settled: false,
            }),
            Admission::Exhausted {
                spent,
                estimate,
                limit,
            } => Err(Admission::Exhausted {
                spent,
                estimate,
                limit,
            }),
        }
    }

    /// Currently reserved in-flight budget across active attempts.
    #[must_use]
    pub fn reserved(&self) -> MicroUsd {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.reserved
    }

    /// Total spend the guard knows about: the ledger's or the reservation
    /// floor, whichever is larger.
    #[must_use]
    pub fn committed(&self) -> MicroUsd {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.committed.max(state.reserved)
    }
}

/// RAII guard for an in-flight budget reservation created by [`BudgetGuard::reserve`].
///
/// Calling [`BudgetReservation::settle`] folds the post-attempt ledger spend into
/// the underlying [`BudgetGuard`] and releases the reservation. If dropped without
/// calling `settle` (for instance when a worker panics or exits early), `Drop`
/// releases the reserved estimate via [`BudgetGuard::release_unsettled`].
#[derive(Debug)]
pub struct BudgetReservation<'a> {
    guard: &'a BudgetGuard,
    estimate: MicroUsd,
    settled: bool,
}

impl BudgetReservation<'_> {
    /// The pessimistic cost estimate held by this reservation.
    #[must_use]
    pub const fn estimate(&self) -> MicroUsd {
        self.estimate
    }

    /// Settle this reservation against the post-attempt ledger spend.
    pub fn settle(mut self, ledger_spent: Spend) {
        self.settled = true;
        self.guard.settle(ledger_spent, self.estimate);
    }
}

impl Drop for BudgetReservation<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.guard.release_unsettled(self.estimate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use crate::pricing::TokenUsage;

    fn flash() -> ModelPrice {
        ModelPrice {
            input_miss: MicroUsd::from_micros(150_000),
            input_hit: MicroUsd::from_micros(3_000),
            output: MicroUsd::from_micros(600_000),
        }
    }

    fn entry(mission: &str, micros: u64) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: mission.into(),
            task: "t-1".into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        })
    }

    #[test]
    fn spend_is_per_mission_and_saturating() {
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        append_entry(&path, &entry("m-1", 100)).unwrap();
        append_entry(&path, &entry("m-1", 200)).unwrap();
        append_entry(&path, &entry("m-2", 50)).unwrap();
        let spend = Budget::spent(&path, "m-1");
        assert_eq!(spend.calls, 2);
        assert_eq!(spend.total, MicroUsd::from_micros(300));
        assert_eq!(Budget::spent(&path, "absent").total, MicroUsd::ZERO);
    }

    #[test]
    fn the_estimate_prices_prompt_and_output_in_full() {
        let budget = Budget::new(MicroUsd::from_micros(1_000_000));
        // 400 chars => 100 input tokens at $0.15/M, 8192 output at $0.60/M.
        let estimate = budget.estimate(flash(), &"x".repeat(400));
        assert_eq!(estimate, MicroUsd::from_micros(4_931));
        // A cache hit can only make the real cost smaller than this estimate.
        assert!(budget.estimate(flash(), "") > MicroUsd::ZERO);
    }

    #[test]
    fn an_attempt_is_refused_when_it_would_cross_the_limit() {
        let budget = Budget::new(MicroUsd::from_micros(100));
        let spent = Spend {
            calls: 1,
            total: MicroUsd::from_micros(90),
        };
        assert!(!budget.admits(spent, MicroUsd::ZERO, MicroUsd::from_micros(11)));
        assert!(budget.admits(spent, MicroUsd::ZERO, MicroUsd::from_micros(10)));
        assert_eq!(budget.remaining(spent), MicroUsd::from_micros(10));
    }

    #[test]
    fn the_warning_line_is_four_fifths() {
        let budget = Budget::new(MicroUsd::from_micros(100));
        let spend = |micros| Spend {
            calls: 1,
            total: MicroUsd::from_micros(micros),
        };
        assert!(!budget.at_risk(spend(79)));
        assert!(budget.at_risk(spend(80)));
    }

    #[test]
    fn a_guard_reserves_across_concurrent_attempts() {
        let guard = BudgetGuard::new(Budget::new(MicroUsd::from_micros(100)));
        let none = Spend::default();
        let ten = MicroUsd::from_micros(10);
        assert_eq!(guard.admit(none, ten), Admission::Allowed);
        assert_eq!(guard.admit(none, ten), Admission::Allowed);
        assert_eq!(
            guard.admit(none, MicroUsd::from_micros(81)),
            Admission::Exhausted {
                spent: MicroUsd::ZERO,
                estimate: MicroUsd::from_micros(81),
                limit: MicroUsd::from_micros(100)
            }
        );
        guard.settle(none, ten);
        assert_eq!(
            guard.admit(none, MicroUsd::from_micros(81)),
            Admission::Allowed
        );
    }

    #[test]
    fn a_guard_folds_in_the_ledgers_charge() {
        let guard = BudgetGuard::new(Budget::new(MicroUsd::from_micros(100)));
        let ten = MicroUsd::from_micros(10);
        assert_eq!(guard.admit(Spend::default(), ten), Admission::Allowed);
        let charged = Spend {
            calls: 1,
            total: MicroUsd::from_micros(95),
        };
        guard.settle(charged, ten);
        assert_eq!(
            guard.admit(Spend::default(), ten),
            Admission::Exhausted {
                spent: MicroUsd::from_micros(95),
                estimate: ten,
                limit: MicroUsd::from_micros(100)
            }
        );
    }

    #[test]
    fn dropping_unsettled_budget_reservation_releases_reserved_budget() {
        let guard = BudgetGuard::new(Budget::new(MicroUsd::from_micros(100)));
        let none = Spend::default();
        let eighty = MicroUsd::from_micros(80);
        let thirty = MicroUsd::from_micros(30);

        let reservation = guard.reserve(none, eighty).expect("80 fits in 100");
        assert_eq!(reservation.estimate(), eighty);
        assert_eq!(guard.reserved(), eighty);
        // While 80 is reserved, another 30 is refused.
        assert!(guard.reserve(none, thirty).is_err());

        // Dropping without settling immediately frees the 80 micro-USD.
        drop(reservation);
        assert_eq!(guard.reserved(), MicroUsd::ZERO);

        // Now a panic inside a worker scope also releases its reservation via Drop.
        let unwind_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = guard.reserve(none, eighty).expect("80 fits after release");
            assert_eq!(guard.reserved(), eighty);
            panic!("simulated worker panic");
        }));
        assert!(unwind_result.is_err());
        assert_eq!(guard.reserved(), MicroUsd::ZERO);

        // Subsequent reservation of 80 succeeds and settling folds in spend without double-releasing.
        let r1 = guard.reserve(none, thirty).expect("30 fits");
        let r2 = guard
            .reserve(none, MicroUsd::from_micros(50))
            .expect("50 fits");
        assert_eq!(guard.reserved(), eighty);
        r1.settle(Spend {
            calls: 1,
            total: MicroUsd::from_micros(25),
        });
        assert_eq!(guard.reserved(), MicroUsd::from_micros(50));
        drop(r2);
        assert_eq!(guard.reserved(), MicroUsd::ZERO);
        assert_eq!(guard.committed(), MicroUsd::from_micros(25));
    }
}
