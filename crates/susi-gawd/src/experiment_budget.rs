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
