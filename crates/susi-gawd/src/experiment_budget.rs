//! Cumulative RSI experiment budgets (VC-201-014).
//!
//! Reserve and debit tokens, wall time, subprocesses, and cloud spend across
//! retries and delegated descendants. Exhausting a budget durably stops new
//! work and cancels eligible children.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BudgetLimits {
    pub tokens: u64,
    pub wall_ms: u64,
    pub subprocesses: u64,
    pub cloud_spend_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BudgetUsage {
    pub tokens: u64,
    pub wall_ms: u64,
    pub subprocesses: u64,
    pub cloud_spend_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentBudget {
    pub id: String,
    pub limits: BudgetLimits,
    pub usage: BudgetUsage,
    pub exhausted: bool,
    pub cancelled_children: BTreeSet<String>,
}

impl ExperimentBudget {
    #[must_use]
    pub fn new(id: &str, limits: BudgetLimits) -> Self {
        Self {
            id: id.to_string(),
            limits,
            usage: BudgetUsage::default(),
            exhausted: false,
            cancelled_children: BTreeSet::new(),
        }
    }

    /// Reserve capacity for a unit of work. Fails (and marks exhausted) when
    /// any dimension would exceed its limit.
    pub fn reserve(&mut self, need: BudgetUsage) -> Result<(), String> {
        if self.exhausted {
            return Err("budget exhausted".into());
        }
        let next = BudgetUsage {
            tokens: self.usage.tokens.saturating_add(need.tokens),
            wall_ms: self.usage.wall_ms.saturating_add(need.wall_ms),
            subprocesses: self.usage.subprocesses.saturating_add(need.subprocesses),
            cloud_spend_micros: self
                .usage
                .cloud_spend_micros
                .saturating_add(need.cloud_spend_micros),
        };
        if next.tokens > self.limits.tokens
            || next.wall_ms > self.limits.wall_ms
            || next.subprocesses > self.limits.subprocesses
            || next.cloud_spend_micros > self.limits.cloud_spend_micros
        {
            self.exhausted = true;
            return Err("budget exhausted".into());
        }
        self.usage = next;
        Ok(())
    }

    /// Debit after a completed unit (already reserved or direct spend).
    pub fn debit(&mut self, used: BudgetUsage) -> Result<(), String> {
        self.reserve(used)
    }

    /// Cancel an eligible child when the parent budget is exhausted.
    pub fn cancel_child(&mut self, child_id: &str) -> Result<(), String> {
        if !self.exhausted {
            return Err("budget still open".into());
        }
        self.cancelled_children.insert(child_id.to_string());
        Ok(())
    }

    #[must_use]
    pub fn allows_new_work(&self) -> bool {
        !self.exhausted
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn sample_limits() -> BudgetLimits {
        BudgetLimits {
            tokens: 1_000,
            wall_ms: 5_000,
            subprocesses: 4,
            cloud_spend_micros: 50_000,
        }
    }

    #[test]
    fn reserve_and_debit_accumulate_usage_within_limits() {
        let mut b = ExperimentBudget::new("exp-1", sample_limits());
        assert!(b.allows_new_work());
        b.reserve(BudgetUsage {
            tokens: 400,
            wall_ms: 1_000,
            subprocesses: 1,
            cloud_spend_micros: 10_000,
        })
        .unwrap();
        b.debit(BudgetUsage {
            tokens: 600,
            wall_ms: 2_000,
            subprocesses: 2,
            cloud_spend_micros: 20_000,
        })
        .unwrap();
        assert_eq!(b.usage.tokens, 1_000);
        assert_eq!(b.usage.subprocesses, 3);
        assert!(b.allows_new_work());
    }

    #[test]
    fn exceeding_any_dimension_marks_budget_exhausted_and_blocks_subsequent_work() {
        let mut b = ExperimentBudget::new("exp-2", sample_limits());
        let err = b
            .reserve(BudgetUsage {
                tokens: 1_001,
                wall_ms: 100,
                subprocesses: 1,
                cloud_spend_micros: 0,
            })
            .unwrap_err();
        assert_eq!(err, "budget exhausted");
        assert!(b.exhausted);
        assert!(!b.allows_new_work());
        assert_eq!(b.usage.tokens, 0);
        assert!(b.reserve(BudgetUsage::default()).is_err());
    }

    #[test]
    fn cancel_child_refuses_while_open_and_records_child_when_exhausted() {
        let mut b = ExperimentBudget::new("exp-3", sample_limits());
        assert_eq!(b.cancel_child("child-a"), Err("budget still open".into()));

        let _ = b.reserve(BudgetUsage {
            tokens: 0,
            wall_ms: 0,
            subprocesses: 5,
            cloud_spend_micros: 0,
        });
        b.cancel_child("child-a").unwrap();
        b.cancel_child("child-b").unwrap();
        assert!(b.cancelled_children.contains("child-a"));
        assert!(b.cancelled_children.contains("child-b"));
    }

    #[test]
    fn saturating_addition_prevents_overflow_bypass_of_limits() {
        let mut b = ExperimentBudget::new("exp-4", sample_limits());
        let res = b.reserve(BudgetUsage {
            tokens: u64::MAX,
            wall_ms: 0,
            subprocesses: 0,
            cloud_spend_micros: 0,
        });
        assert!(res.is_err());
        assert!(b.exhausted);
    }
}
