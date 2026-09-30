//! Per-vendor daily spend tracking with hard caps.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendLimits {
    /// vendor -> USD hard cap for the day
    pub daily_usd: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendTracker {
    /// (day, vendor) -> USD spent
    ledger: BTreeMap<(String, String), f64>,
    limits: BTreeMap<String, f64>,
}

impl SpendTracker {
    #[must_use]
    pub fn with_limits(limits: SpendLimits) -> Self {
        Self {
            ledger: BTreeMap::new(),
            limits: limits.daily_usd,
        }
    }

    pub fn record(&mut self, day: &str, vendor: &str, usd: f64) {
        *self
            .ledger
            .entry((day.to_string(), vendor.to_string()))
            .or_default() += usd;
    }

    #[must_use]
    pub fn spent(&self, day: &str, vendor: &str) -> f64 {
        self.ledger
            .get(&(day.to_string(), vendor.to_string()))
            .copied()
            .unwrap_or(0.0)
    }

    /// Whether routing may still send work to `vendor` today.
    #[must_use]
    pub fn under_cap(&self, day: &str, vendor: &str) -> bool {
        match self.limits.get(vendor) {
            Some(&cap) => self.spent(day, vendor) < cap,
            None => true,
        }
    }

    /// Reserve estimated spend; fails if it would cross the hard cap.
    pub fn reserve(&mut self, day: &str, vendor: &str, estimate_usd: f64) -> Result<(), String> {
        let next = self.spent(day, vendor) + estimate_usd;
        if let Some(&cap) = self.limits.get(vendor) {
            if next > cap {
                return Err(format!("vendor {vendor} daily hard cap {cap} reached"));
            }
        }
        self.record(day, vendor, estimate_usd);
        Ok(())
    }

    #[must_use]
    pub fn route_away_vendors(&self, day: &str) -> Vec<String> {
        self.limits
            .keys()
            .filter(|v| !self.under_cap(day, v))
            .cloned()
            .collect()
    }
}
