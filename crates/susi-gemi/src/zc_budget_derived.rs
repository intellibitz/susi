//! Budget dial derived from context; ask only on ambiguity.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetDial {
    pub daily_usd: f64,
    pub asked_user: bool,
}

#[must_use]
pub fn derive_budget(context_tokens: u64, ambiguous: bool) -> BudgetDial {
    if ambiguous {
        return BudgetDial {
            daily_usd: 0.0,
            asked_user: true,
        };
    }
    let daily = if context_tokens < 8_000 {
        5.0
    } else if context_tokens < 32_000 {
        20.0
    } else {
        50.0
    };
    BudgetDial {
        daily_usd: daily,
        asked_user: false,
    }
}

#[cfg(test)]
mod zc_budget_derived_tests {
    use super::*;

    #[test]
    fn zc_budget_derived_from_context_asks_only_when_ambiguous() {
        let b = derive_budget(4_000, false);
        assert_eq!(b.daily_usd, 5.0);
        assert!(!b.asked_user);
        let a = derive_budget(0, true);
        assert!(a.asked_user);
    }
}
