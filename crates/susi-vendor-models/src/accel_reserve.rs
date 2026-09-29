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

    pub fn admit(&mut self, need: u64) -> AdmitDecision {
        if need == 0 || need > self.free() {
            return AdmitDecision::RejectedInsufficient;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(need);
        AdmitDecision::Admitted { reservation: need }
    }

    pub fn release(&mut self, reservation: u64) {
        self.reserved_bytes = self.reserved_bytes.saturating_sub(reservation);
    }
}
