//! A/B experiments with statistical tests on routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbResult {
    pub winner: Option<String>,
    pub significant: bool,
}

/// Simple two-proportion z-test style gate (threshold on rate delta).
#[must_use]
pub fn ab_decide(a_name: &str, a_ok: u32, a_n: u32, b_name: &str, b_ok: u32, b_n: u32) -> AbResult {
    if a_n == 0 || b_n == 0 {
        return AbResult {
            winner: None,
            significant: false,
        };
    }
    let ra = f64::from(a_ok) / f64::from(a_n);
    let rb = f64::from(b_ok) / f64::from(b_n);
    let delta = (ra - rb).abs();
    let significant = delta >= 0.1 && a_n.min(b_n) >= 20;
    let winner = if !significant {
        None
    } else if ra > rb {
        Some(a_name.into())
    } else {
        Some(b_name.into())
    };
    AbResult {
        winner,
        significant,
    }
}

#[cfg(test)]
mod brain_experiments_tests {
    use super::*;

    #[test]
    fn brain_experiments_needs_enough_samples() {
        assert!(!ab_decide("a", 9, 10, "b", 1, 10).significant);
        let r = ab_decide("a", 18, 20, "b", 4, 20);
        assert!(r.significant);
        assert_eq!(r.winner.as_deref(), Some("a"));
    }
}
