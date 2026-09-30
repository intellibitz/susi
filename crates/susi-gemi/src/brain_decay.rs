//! Time-decay of brain evidence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecayedScore {
    pub weight: f64,
}

/// Exponential decay of an evidence weight by age.
#[must_use]
pub fn decay_weight(raw: f64, age_secs: u64, half_life_secs: u64) -> DecayedScore {
    let hl = half_life_secs.max(1) as f64;
    let age = age_secs as f64;
    let weight = raw * 0.5_f64.powf(age / hl);
    DecayedScore { weight }
}

#[cfg(test)]
mod brain_decay_tests {
    use super::*;

    #[test]
    fn brain_decay_halves_at_half_life() {
        let d = decay_weight(1.0, 3600, 3600);
        assert!((d.weight - 0.5).abs() < 1e-9);
        assert!(decay_weight(1.0, 0, 3600).weight > decay_weight(1.0, 7200, 3600).weight);
    }
}
