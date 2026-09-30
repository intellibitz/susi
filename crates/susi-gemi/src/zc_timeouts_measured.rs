//! Cloud and provisioning timeouts from measured latency.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasuredTimeouts {
    pub cloud_scout_timeout_secs: u64,
    pub model_provisioning_wait_secs: u64,
}

/// Derive scout and provision waits from RTT (ms) and bandwidth (MiB/s).
#[must_use]
pub fn timeouts_from_measurements(rtt_ms: u64, bandwidth_mib_s: f64) -> MeasuredTimeouts {
    let rtt_secs = (rtt_ms / 1000).max(1);
    let scout = (rtt_secs * 5).clamp(5, 120);
    let bw = bandwidth_mib_s.max(0.1);
    // Assume a 4 GiB model download as the provision upper bound.
    let provision = ((4096.0 / bw) as u64).saturating_mul(2).clamp(60, 7200);
    MeasuredTimeouts {
        cloud_scout_timeout_secs: scout,
        model_provisioning_wait_secs: provision,
    }
}

#[cfg(test)]
mod zc_timeouts_measured_tests {
    use super::*;

    #[test]
    fn zc_timeouts_measured_from_rtt_and_bandwidth() {
        let slow = timeouts_from_measurements(800, 1.0);
        let fast = timeouts_from_measurements(40, 100.0);
        assert!(slow.cloud_scout_timeout_secs >= fast.cloud_scout_timeout_secs);
        assert!(slow.model_provisioning_wait_secs > fast.model_provisioning_wait_secs);
        assert!(fast.cloud_scout_timeout_secs >= 5);
    }
}
