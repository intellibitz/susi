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

/// Live accelerator probe: free VRAM across all NVIDIA GPUs in MiB, via
/// `nvidia-smi` — `None` when no probe exists (no GPU, no driver, no
/// binary). Callers must treat `None` as unmeasured, never as capacity
/// to assume: accelerator-hungry work fails closed when headroom cannot
/// be observed.
#[must_use]
pub fn probe_vram_free_mb() -> Option<u64> {
    let out = susi_paths::console::background_command("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut total = 0u64;
    let mut seen = false;
    for line in text.lines() {
        if let Ok(mb) = line.trim().parse::<u64>() {
            total = total.saturating_add(mb);
            seen = true;
        }
    }
    seen.then_some(total)
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

    #[test]
    fn admit_rejects_zero_or_over_free_and_release_saturates_at_zero() {
        let mut pool = AccelPool {
            total_bytes: 1000,
            reserved_bytes: 200,
        };
        assert_eq!(pool.admit(0), AdmitDecision::RejectedInsufficient);
        assert_eq!(
            pool.admit(pool.free() + 1),
            AdmitDecision::RejectedInsufficient
        );
        pool.release(999_999);
        assert_eq!(pool.reserved_bytes, 0);
        assert_eq!(pool.free(), 1000);
    }

    #[test]
    fn accel_pool_free_and_serde_json_roundtrip_preserve_state() {
        let pool = AccelPool {
            total_bytes: 2000,
            reserved_bytes: 500,
        };
        assert_eq!(pool.free(), 1500);
        assert_eq!(pool.headroom(), 100);
        let json = serde_json::to_string(&pool).expect("serialize");
        let back: AccelPool = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, pool);
    }

    #[test]
    fn sequential_admissions_and_partial_releases_maintain_exact_headroom_bounds() {
        let mut pool = AccelPool {
            total_bytes: 1000,
            reserved_bytes: 0,
        };
        assert_eq!(
            pool.admit(300),
            AdmitDecision::Admitted { reservation: 300 }
        );
        assert_eq!(
            pool.admit(400),
            AdmitDecision::Admitted { reservation: 400 }
        );
        assert_eq!(pool.free(), 300);

        pool.release(300);
        assert_eq!(pool.reserved_bytes, 400);
        assert_eq!(
            pool.admit(550),
            AdmitDecision::Admitted { reservation: 550 }
        );
        assert_eq!(pool.free(), 50);
        assert_eq!(pool.admit(1), AdmitDecision::RejectedInsufficient);
    }
}
