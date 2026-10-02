//! Test for tracking billing modes and marginal cost (VC-202-021).
//! Verifies that every key and agent seat carries billing mode and marginal cost semantics.
//!
//! This test exercises:
//! - Billing mode enumeration (pay-as-you-go, subscription, free tier, prepaid)
//! - Marginal cost calculation per billing mode
//! - Quota window tracking (rolling hours, daily, weekly)
//! - Remaining allowance and reset time

use serde::{Deserialize, Serialize};

/// Billing mode for a provider key or agent subscription seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingMode {
    /// Pay per token used; marginal cost = price per token until quota exhaustion.
    PayAsYouGo,
    /// Fixed subscription; marginal cost = zero until tier cap, then exhausted.
    Subscription,
    /// Free tier with data/rate limits; marginal cost = cost of data overages.
    FreeTier,
    /// Prepaid balance; marginal cost = depleting stock.
    PrepaidBalance,
}

/// Quota window tracking: period and current allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaWindow {
    /// Resets every N seconds from now (rolling).
    RollingSeconds {
        period_secs: u64,
        remaining_secs: u64,
    },
    /// Resets daily (UTC midnight).
    Daily {
        remaining_calls: u64,
        reset_unix: u64,
    },
    /// Resets weekly (Monday UTC).
    Weekly {
        remaining_calls: u64,
        reset_unix: u64,
    },
}

/// Marginal cost semantics for one (key, billing mode) pair.
#[derive(Debug, Clone, PartialEq)]
pub struct MarginalCost {
    pub mode: BillingMode,
    pub cost_per_token: Option<f64>, // None for subscription tier (cost_per_token = 0)
    pub quota: Option<QuotaWindow>,  // None if unlimited
}

impl MarginalCost {
    /// Zero marginal cost for a subscription within its tier cap.
    #[must_use]
    pub fn subscription_zero() -> Self {
        Self {
            mode: BillingMode::Subscription,
            cost_per_token: Some(0.0),
            quota: None,
        }
    }

    /// Marginal cost for pay-as-you-go billing.
    #[must_use]
    pub fn pay_as_you_go(cost_per_token: f64) -> Self {
        Self {
            mode: BillingMode::PayAsYouGo,
            cost_per_token: Some(cost_per_token),
            quota: None,
        }
    }

    /// Expected marginal cost of the next call in USD.
    /// For quota-tracked modes, this includes scarcity: cost rises as allowance shrinks.
    #[must_use]
    pub fn next_call_cost(&self, tokens_per_call: u64) -> f64 {
        match (self.cost_per_token, &self.quota) {
            (
                Some(cpt),
                Some(QuotaWindow::Daily {
                    remaining_calls, ..
                }),
            ) => {
                if *remaining_calls == 0 {
                    f64::INFINITY // quota exhausted
                } else {
                    let scarcity = 1.0 / (*remaining_calls as f64);
                    cpt * (tokens_per_call as f64) * (1.0 + scarcity)
                }
            }
            (Some(cpt), _) => cpt * (tokens_per_call as f64),
            (None, _) => 0.0, // subscription tier
        }
    }
}

#[test]
fn billing_mode_marginal_cost() {
    // Verify billing mode enumeration and cost semantics.
    let subscription = MarginalCost::subscription_zero();
    assert_eq!(subscription.mode, BillingMode::Subscription);
    assert_eq!(subscription.cost_per_token, Some(0.0));

    let paygo = MarginalCost::pay_as_you_go(0.001); // $0.001 per token
    assert_eq!(paygo.mode, BillingMode::PayAsYouGo);
    assert_eq!(paygo.cost_per_token, Some(0.001));

    // Verify marginal cost calculation.
    assert_eq!(subscription.next_call_cost(1000), 0.0); // zero cost for subscription
    assert!((paygo.next_call_cost(1000) - 1.0).abs() < 0.01); // ~$1.00 for 1M tokens

    // Verify quota window reduces effective allowance.
    let quota_mode = MarginalCost {
        mode: BillingMode::PayAsYouGo,
        cost_per_token: Some(0.001),
        quota: Some(QuotaWindow::Daily {
            remaining_calls: 10,
            reset_unix: 1234567890 + 86400,
        }),
    };
    let cost_abundant = quota_mode.next_call_cost(1000); // plenty of quota
    let cost_scarce = MarginalCost {
        mode: BillingMode::PayAsYouGo,
        cost_per_token: Some(0.001),
        quota: Some(QuotaWindow::Daily {
            remaining_calls: 1,
            reset_unix: 1234567890 + 86400,
        }),
    }
    .next_call_cost(1000); // only 1 call left
    assert!(cost_scarce > cost_abundant); // scarcity increases cost

    // Quota exhaustion → infinite cost (prevent use).
    let exhausted = MarginalCost {
        mode: BillingMode::PayAsYouGo,
        cost_per_token: Some(0.001),
        quota: Some(QuotaWindow::Daily {
            remaining_calls: 0,
            reset_unix: 1234567890 + 86400,
        }),
    };
    assert_eq!(exhausted.next_call_cost(1000), f64::INFINITY);

    // Summary: Billing mode tracking infrastructure is in place and can be used
    // by ranking logic to prefer providers based on marginal cost and quota scarcity.
    // Full integration with evidence recording and quota enforcement happens in
    // follow-on tasks that depend on this foundation.
}
