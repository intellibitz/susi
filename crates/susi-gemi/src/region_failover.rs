//! Regional endpoint latency probing, Hayashibara $\varphi$-accrual failure
//! detection, and tail-latency ($p_{95}$) hedged failover routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionLatency {
    pub region: String,
    pub rtt_ms: u64,
}

/// Pick the lowest-RTT healthy region.
#[must_use]
pub fn failover(regions: &[RegionLatency], unhealthy: &[&str]) -> Option<String> {
    regions
        .iter()
        .filter(|r| !unhealthy.iter().any(|u| *u == r.region))
        .min_by_key(|r| r.rtt_ms)
        .map(|r| r.region.clone())
}

/// Rich regional telemetry with EWMA mean RTT, RTT standard deviation (jitter),
/// and heartbeat inter-arrival statistics for $\varphi$-accrual failure detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionTelemetry {
    pub region: String,
    pub mean_rtt_ms: f64,
    pub stddev_rtt_ms: f64,
    /// Elapsed milliseconds since last successful response/heartbeat.
    pub elapsed_since_ok_ms: f64,
    /// Mean inter-arrival interval between responses/heartbeats (ms).
    pub heartbeat_mean_ms: f64,
    /// Standard deviation of inter-arrival intervals (ms).
    pub heartbeat_stddev_ms: f64,
}

impl RegionTelemetry {
    /// Normal 95th-percentile tail latency bound: $p_{95} = \mu + 1.64485 \cdot \sigma$.
    #[must_use]
    pub fn p95_rtt_ms(&self) -> f64 {
        (self.mean_rtt_ms + 1.644_853_6 * self.stddev_rtt_ms.max(0.0)).max(1.0)
    }

    /// Hayashibara $\varphi$-accrual suspicion level:
    /// $\varphi(t) = -\log_{10}(1 - \Phi(\frac{t - \mu}{\sigma}))$.
    #[must_use]
    pub fn phi_suspicion(&self) -> f64 {
        phi_accrual(
            self.elapsed_since_ok_ms,
            self.heartbeat_mean_ms,
            self.heartbeat_stddev_ms,
        )
    }
}

/// Compute the Hayashibara $\varphi$-accrual suspicion score using Abramowitz &
/// Stegun 7.1.26 rational approximation for the Gaussian tail probability $Q(z)$.
#[must_use]
pub fn phi_accrual(elapsed_ms: f64, mean_ms: f64, stddev_ms: f64) -> f64 {
    let sigma = stddev_ms.max(mean_ms * 0.1).max(10.0);
    if elapsed_ms <= mean_ms {
        return 0.0;
    }
    let z = (elapsed_ms - mean_ms) / sigma;
    // Standard normal upper tail Q(z) = 0.5 * erfc(z / sqrt(2))
    let x = z / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let q = (0.5 * poly * (-x * x).exp()).clamp(1e-16, 1.0);
    (-q.log10()).max(0.0)
}

/// Tail-latency hedged routing plan across regional endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HedgedRoutePlan {
    pub primary_region: String,
    pub primary_p95_ms: f64,
    pub primary_phi: f64,
    /// Optional backup region to fire a hedged request to if primary exceeds `hedge_after_ms`.
    pub hedge_region: Option<String>,
    pub hedge_after_ms: u64,
}

/// Select the optimal primary and hedged backup region among candidates whose
/// $\varphi$-accrual suspicion is below `phi_threshold` (typically `8.0`).
#[must_use]
pub fn plan_hedged_failover(
    regions: &[RegionTelemetry],
    unhealthy: &[&str],
    phi_threshold: f64,
) -> Option<HedgedRoutePlan> {
    let mut healthy: Vec<&RegionTelemetry> = regions
        .iter()
        .filter(|r| !unhealthy.iter().any(|u| *u == r.region))
        .filter(|r| r.phi_suspicion() < phi_threshold)
        .collect();
    if healthy.is_empty() {
        return None;
    }
    // Rank by p95 tail latency plus a soft penalty for rising phi suspicion.
    healthy.sort_by(|a, b| {
        let score_a = a.p95_rtt_ms() * (1.0 + 0.1 * a.phi_suspicion());
        let score_b = b.p95_rtt_ms() * (1.0 + 0.1 * b.phi_suspicion());
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let primary = healthy[0];
    let hedge = healthy.get(1).map(|r| r.region.clone());
    let hedge_after_ms = (primary.p95_rtt_ms().ceil() as u64).max(25);
    Some(HedgedRoutePlan {
        primary_region: primary.region.clone(),
        primary_p95_ms: primary.p95_ms_rounded(),
        primary_phi: primary.phi_suspicion(),
        hedge_region: hedge,
        hedge_after_ms,
    })
}

impl RegionTelemetry {
    fn p95_ms_rounded(&self) -> f64 {
        (self.p95_rtt_ms() * 100.0).round() / 100.0
    }
}

#[cfg(test)]
mod region_failover_tests {
    use super::*;

    #[test]
    fn region_failover_picks_lowest_rtt() {
        let regs = [
            RegionLatency {
                region: "us-east".into(),
                rtt_ms: 80,
            },
            RegionLatency {
                region: "eu-west".into(),
                rtt_ms: 40,
            },
            RegionLatency {
                region: "ap-south".into(),
                rtt_ms: 120,
            },
        ];
        assert_eq!(failover(&regs, &[]).as_deref(), Some("eu-west"));
        assert_eq!(failover(&regs, &["eu-west"]).as_deref(), Some("us-east"));
    }

    #[test]
    fn phi_accrual_and_p95_hedged_failover_avoid_high_jitter_and_stale_regions() {
        let regs = [
            // Low mean (45ms) but extreme tail jitter (stddev=60ms -> p95=143.7ms)
            RegionTelemetry {
                region: "us-east-jittery".into(),
                mean_rtt_ms: 45.0,
                stddev_rtt_ms: 60.0,
                elapsed_since_ok_ms: 200.0,
                heartbeat_mean_ms: 1000.0,
                heartbeat_stddev_ms: 100.0,
            },
            // Slightly higher mean (55ms) but rock-solid p95 (stddev=5ms -> p95=63.2ms)
            RegionTelemetry {
                region: "eu-west-stable".into(),
                mean_rtt_ms: 55.0,
                stddev_rtt_ms: 5.0,
                elapsed_since_ok_ms: 200.0,
                heartbeat_mean_ms: 1000.0,
                heartbeat_stddev_ms: 100.0,
            },
            // Dead region (elapsed 10,000ms >> heartbeat 1,000ms -> phi > 8.0)
            RegionTelemetry {
                region: "ap-south-dead".into(),
                mean_rtt_ms: 20.0,
                stddev_rtt_ms: 2.0,
                elapsed_since_ok_ms: 10_000.0,
                heartbeat_mean_ms: 1000.0,
                heartbeat_stddev_ms: 100.0,
            },
        ];
        assert!(regs[2].phi_suspicion() > 8.0);
        let plan = plan_hedged_failover(&regs, &[], 8.0).expect("must find healthy region");
        assert_eq!(plan.primary_region, "eu-west-stable");
        assert_eq!(plan.hedge_region.as_deref(), Some("us-east-jittery"));
        assert_eq!(plan.hedge_after_ms, 64);
    }

    #[test]
    fn failover_and_hedged_plan_return_none_when_all_regions_unhealthy_or_stale() {
        let regs = [
            RegionLatency {
                region: "us-east".into(),
                rtt_ms: 50,
            },
            RegionLatency {
                region: "eu-west".into(),
                rtt_ms: 40,
            },
        ];
        assert!(failover(&regs, &["us-east", "eu-west"]).is_none());

        let stale_telemetry = [RegionTelemetry {
            region: "us-east".into(),
            mean_rtt_ms: 30.0,
            stddev_rtt_ms: 5.0,
            elapsed_since_ok_ms: 15_000.0,
            heartbeat_mean_ms: 1000.0,
            heartbeat_stddev_ms: 100.0,
        }];
        assert!(plan_hedged_failover(&stale_telemetry, &[], 8.0).is_none());
        assert!(plan_hedged_failover(&stale_telemetry, &["us-east"], 100.0).is_none());
    }

    #[test]
    fn phi_accrual_is_zero_within_mean_and_increases_monotonically_beyond() {
        assert!((phi_accrual(500.0, 1000.0, 100.0) - 0.0).abs() < f64::EPSILON);
        assert!((phi_accrual(1000.0, 1000.0, 100.0) - 0.0).abs() < f64::EPSILON);

        let p1 = phi_accrual(1200.0, 1000.0, 100.0);
        let p2 = phi_accrual(1500.0, 1000.0, 100.0);
        let p3 = phi_accrual(2000.0, 1000.0, 100.0);
        assert!(p1 > 0.0);
        assert!(p2 > p1);
        assert!(p3 > p2);
    }

    #[test]
    fn p95_rtt_ms_clamps_negative_stddev_and_enforces_one_ms_floor() {
        let tiny = RegionTelemetry {
            region: "local-edge".into(),
            mean_rtt_ms: 0.2,
            stddev_rtt_ms: -5.0,
            elapsed_since_ok_ms: 10.0,
            heartbeat_mean_ms: 1000.0,
            heartbeat_stddev_ms: 50.0,
        };
        assert!((tiny.p95_rtt_ms() - 1.0).abs() < f64::EPSILON);

        let plan = plan_hedged_failover(&[tiny], &[], 8.0).expect("single healthy region");
        assert_eq!(plan.primary_region, "local-edge");
        assert_eq!(plan.hedge_region, None);
        assert_eq!(plan.hedge_after_ms, 25);
    }
}
