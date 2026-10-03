//! Reserve accelerator memory before model admission (VC-201-043).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccelPool {
    pub total_bytes: u64,
    pub reserved_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitDecision {
    Admitted { reservation: u64 },
    RejectedInsufficient,
}

impl AccelPool {
    #[must_use]
    pub fn free(&self) -> u64 {
        self.total_bytes.saturating_sub(self.reserved_bytes)
    }

    #[must_use]
    pub fn headroom(&self) -> u64 {
        self.total_bytes / 20
    }

    pub fn admit(&mut self, need: u64) -> AdmitDecision {
        if need == 0 || need > self.free() {
            return AdmitDecision::RejectedInsufficient;
        }

        let headroom = self.headroom();
        if self.free() - need < headroom {
            return AdmitDecision::RejectedInsufficient;
        }

        self.reserved_bytes = self.reserved_bytes.saturating_add(need);
        AdmitDecision::Admitted { reservation: need }
    }

    pub fn release(&mut self, reservation: u64) {
        self.reserved_bytes = self.reserved_bytes.saturating_sub(reservation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headroom_calculation() {
        let pool = AccelPool {
            total_bytes: 1000,
            reserved_bytes: 0,
        };
        assert_eq!(pool.headroom(), 50);
    }

    #[test]
    fn test_admit_with_headroom() {
        let mut pool = AccelPool {
            total_bytes: 1000,
            reserved_bytes: 0,
        };

        // Need 900, free is 1000, headroom is 50.
        // 1000 - 900 = 100 >= 50. Should be admitted.
        assert_eq!(
            pool.admit(900),
            AdmitDecision::Admitted { reservation: 900 }
        );
        assert_eq!(pool.reserved_bytes, 900);

        // Now free is 100, headroom is 50.
        // Need 60. 100 - 60 = 40 < 50. Should be rejected.
        assert_eq!(pool.admit(60), AdmitDecision::RejectedInsufficient);
        assert_eq!(pool.reserved_bytes, 900);

        // Need 50. 100 - 50 = 50 >= 50. Should be admitted.
        assert_eq!(pool.admit(50), AdmitDecision::Admitted { reservation: 50 });
        assert_eq!(pool.reserved_bytes, 950);
    }
}
