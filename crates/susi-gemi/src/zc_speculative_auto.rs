//! Speculative decoding enabled when a draft model exists.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeculativePlan {
    pub enabled: bool,
    pub draft_tokens: u32,
}

/// Enable speculative decoding when a draft model is present and measured
/// speedup exceeds break-even.
#[must_use]
pub fn speculative_plan(draft_available: bool, measured_speedup: f64) -> SpeculativePlan {
    if !draft_available || measured_speedup < 1.15 {
        return SpeculativePlan {
            enabled: false,
            draft_tokens: 0,
        };
    }
    let draft_tokens = if measured_speedup >= 1.8 { 8 } else { 4 };
    SpeculativePlan {
        enabled: true,
        draft_tokens,
    }
}

#[cfg(test)]
mod zc_speculative_auto_tests {
    use super::*;

    #[test]
    fn zc_speculative_auto_enables_when_draft_and_speedup() {
        assert!(!speculative_plan(false, 2.0).enabled);
        assert!(!speculative_plan(true, 1.0).enabled);
        let on = speculative_plan(true, 2.0);
        assert!(on.enabled);
        assert_eq!(on.draft_tokens, 8);
    }
}
