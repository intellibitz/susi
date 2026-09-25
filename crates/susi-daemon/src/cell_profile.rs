//! Cell self-profile (Swarm OS Bullet 20)
//!
//! Latency, error rate, and token usage reported back to the daemon.
//! Distinct from cost breakdowns, which price the same tokens.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellProfile {
    pub samples: u64,
    pub errors: u64,
    pub total_latency_ms: u64,
    pub tokens: u64,
}

impl CellProfile {
    pub fn record(&mut self, latency_ms: u64, failed: bool, tokens: u64) {
        self.samples = self.samples.saturating_add(1);
        if failed {
            self.errors = self.errors.saturating_add(1);
        }
        self.total_latency_ms = self.total_latency_ms.saturating_add(latency_ms);
        self.tokens = self.tokens.saturating_add(tokens);
    }

    /// Errors per million samples. `None` until at least one sample exists.
    pub fn error_rate_ppm(&self) -> Option<u64> {
        self.errors
            .checked_mul(1_000_000)
            .and_then(|scaled| scaled.checked_div(self.samples))
    }

    pub fn avg_latency_ms(&self) -> Option<u64> {
        self.total_latency_ms.checked_div(self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_tracks_latency_errors_and_tokens() {
        let mut profile = CellProfile::default();
        assert!(profile.error_rate_ppm().is_none());
        profile.record(10, false, 100);
        profile.record(30, true, 50);
        assert_eq!(profile.tokens, 150);
        assert_eq!(profile.avg_latency_ms(), Some(20));
        assert_eq!(profile.error_rate_ppm(), Some(500_000));
    }
}
