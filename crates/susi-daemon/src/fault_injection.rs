//! Chaos Engineering: Fault Injection (Swarm OS Bullet 11)
//!
//! Lets tests and staged rollouts exercise failure paths deliberately:
//! `should_drop_packet` samples a real random byte against a configured
//! drop probability rather than a hardcoded outcome.

pub struct ChaosMonkey {
    drop_probability_pct: u8,
}

impl Default for ChaosMonkey {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ChaosMonkey {
    /// `drop_probability_pct` is clamped to `[0, 100]`.
    pub fn new(drop_probability_pct: u8) -> Self {
        Self {
            drop_probability_pct: drop_probability_pct.min(100),
        }
    }

    /// Samples one byte of randomness and drops if it falls within the
    /// configured probability. Fails closed (never drops) if the entropy
    /// source is unavailable.
    pub fn should_drop_packet(&self) -> bool {
        if self.drop_probability_pct == 0 {
            return false;
        }
        let mut byte = [0u8; 1];
        if getrandom::fill(&mut byte).is_err() {
            return false;
        }
        (u32::from(byte[0]) * 100 / 255) < u32::from(self.drop_probability_pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_probability_never_drops() {
        let chaos = ChaosMonkey::new(0);
        for _ in 0..64 {
            assert!(!chaos.should_drop_packet());
        }
    }

    #[test]
    fn full_probability_always_drops() {
        let chaos = ChaosMonkey::new(100);
        for _ in 0..64 {
            assert!(chaos.should_drop_packet());
        }
    }

    #[test]
    fn probability_is_clamped() {
        let chaos = ChaosMonkey::new(255);
        assert!(chaos.should_drop_packet());
    }
}
