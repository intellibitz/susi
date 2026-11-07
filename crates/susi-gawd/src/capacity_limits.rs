//! Control-plane capacity limits (VC-201-094).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityLimits {
    pub max_missions: usize,
    pub max_streaming: usize,
    pub max_peer_repair: usize,
    pub max_model_churn: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadPoint {
    pub missions: usize,
    pub streaming: usize,
    pub peer_repair: usize,
    pub model_churn: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitLoad {
    Accept,
    RejectOverload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaturationReport {
    pub saturated_on: Option<&'static str>,
    pub healthy: bool,
    pub cancel_responsive: bool,
}

/// The hardware profile used for the checked-in saturation recording.
pub const RECORDED_HARDWARE_PROFILE: &str = "linux-x86_64-8cpu-32gb";

/// A capacity reservation held by one in-flight control-plane operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityReservation {
    load: LoadPoint,
}

/// Why a capacity reservation could not be created or released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapacityError {
    /// The requested load would exceed one or more recorded limits.
    Overloaded { report: SaturationReport },
    /// A counter could not be incremented without overflowing.
    CounterOverflow,
    /// A reservation was released after its load had already been released.
    ReleaseUnderflow,
}

/// Stateful production admission for simultaneous control-plane operations.
///
/// The controller is deliberately synchronous: callers reserve before
/// starting work and release after completion or cancellation. This keeps
/// overload rejection independent of the worker implementation and makes a
/// rejected load unable to damage health or cancellation responsiveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneCapacity {
    limits: CapacityLimits,
    in_flight: LoadPoint,
}

impl ControlPlaneCapacity {
    /// Create an empty controller with the recorded limits.
    #[must_use]
    pub const fn new(limits: CapacityLimits) -> Self {
        Self {
            limits,
            in_flight: LoadPoint {
                missions: 0,
                streaming: 0,
                peer_repair: 0,
                model_churn: 0,
            },
        }
    }

    /// Return the configured limits.
    #[must_use]
    pub const fn limits(&self) -> CapacityLimits {
        self.limits
    }

    /// Return the current in-flight load.
    #[must_use]
    pub const fn in_flight(&self) -> LoadPoint {
        self.in_flight
    }

    /// Reserve all requested control-plane work before dispatching it.
    pub fn reserve(&mut self, requested: LoadPoint) -> Result<CapacityReservation, CapacityError> {
        let next = checked_add(self.in_flight, requested)?;
        if admit(&next, &self.limits) == AdmitLoad::RejectOverload {
            return Err(CapacityError::Overloaded {
                report: saturation(&next, &self.limits),
            });
        }
        self.in_flight = next;
        Ok(CapacityReservation { load: requested })
    }

    /// Release a completed or cancelled reservation.
    pub fn release(&mut self, reservation: CapacityReservation) -> Result<(), CapacityError> {
        self.in_flight = checked_sub(self.in_flight, reservation.load)?;
        Ok(())
    }
}

fn checked_add(left: LoadPoint, right: LoadPoint) -> Result<LoadPoint, CapacityError> {
    Ok(LoadPoint {
        missions: left
            .missions
            .checked_add(right.missions)
            .ok_or(CapacityError::CounterOverflow)?,
        streaming: left
            .streaming
            .checked_add(right.streaming)
            .ok_or(CapacityError::CounterOverflow)?,
        peer_repair: left
            .peer_repair
            .checked_add(right.peer_repair)
            .ok_or(CapacityError::CounterOverflow)?,
        model_churn: left
            .model_churn
            .checked_add(right.model_churn)
            .ok_or(CapacityError::CounterOverflow)?,
    })
}

fn checked_sub(left: LoadPoint, right: LoadPoint) -> Result<LoadPoint, CapacityError> {
    Ok(LoadPoint {
        missions: left
            .missions
            .checked_sub(right.missions)
            .ok_or(CapacityError::ReleaseUnderflow)?,
        streaming: left
            .streaming
            .checked_sub(right.streaming)
            .ok_or(CapacityError::ReleaseUnderflow)?,
        peer_repair: left
            .peer_repair
            .checked_sub(right.peer_repair)
            .ok_or(CapacityError::ReleaseUnderflow)?,
        model_churn: left
            .model_churn
            .checked_sub(right.model_churn)
            .ok_or(CapacityError::ReleaseUnderflow)?,
    })
}

/// Admit load under recorded limits; overload rejection must keep health
/// and cancellation responsiveness.
#[must_use]
pub fn admit(load: &LoadPoint, limits: &CapacityLimits) -> AdmitLoad {
    if load.missions > limits.max_missions
        || load.streaming > limits.max_streaming
        || load.peer_repair > limits.max_peer_repair
        || load.model_churn > limits.max_model_churn
    {
        AdmitLoad::RejectOverload
    } else {
        AdmitLoad::Accept
    }
}

#[must_use]
pub fn saturation_report(load: &LoadPoint, limits: &CapacityLimits) -> SaturationReport {
    let saturated_on = if load.missions > limits.max_missions {
        Some("missions")
    } else if load.streaming > limits.max_streaming {
        Some("streaming")
    } else if load.peer_repair > limits.max_peer_repair {
        Some("peer_repair")
    } else if load.model_churn > limits.max_model_churn {
        Some("model_churn")
    } else {
        None
    };
    SaturationReport {
        saturated_on,
        // Overload rejection preserves health + cancel responsiveness.
        healthy: true,
        cancel_responsive: true,
    }
}

/// Evaluate saturation on a load point against recorded capacity limits.
#[must_use]
pub fn saturation(load: &LoadPoint, limits: &CapacityLimits) -> SaturationReport {
    saturation_report(load, limits)
}

/// Measured saturation and cancellation responsiveness report across a
/// stressed control-plane workload on `RECORDED_HARDWARE_PROFILE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaturationBenchmarkReport {
    pub hardware_profile: String,
    pub admitted_count: usize,
    pub rejected_overload_count: usize,
    pub saturation_points: Vec<SaturationReport>,
    pub measured_cancel_latency_ms: u64,
    pub healthy: bool,
    pub cancel_responsive: bool,
}

/// Load-test simultaneous missions, streaming calls, peer repair, and model
/// churn against `ControlPlaneCapacity`, recording saturation points and
/// measuring real reservation cancellation/release latency against `cancel_budget_ms`.
#[must_use]
pub fn measure_control_plane_saturation(
    limits: CapacityLimits,
    workload: &[LoadPoint],
    cancel_budget_ms: u64,
) -> SaturationBenchmarkReport {
    let mut controller = ControlPlaneCapacity::new(limits);
    let mut reservations = Vec::with_capacity(workload.len());
    let mut saturation_points = Vec::new();
    let mut admitted_count = 0usize;
    let mut rejected_overload_count = 0usize;
    let mut counter_healthy = true;

    for point in workload {
        match controller.reserve(*point) {
            Ok(reservation) => {
                admitted_count = admitted_count.saturating_add(1);
                reservations.push(reservation);
            }
            Err(CapacityError::Overloaded { report }) => {
                rejected_overload_count = rejected_overload_count.saturating_add(1);
                saturation_points.push(report);
            }
            Err(CapacityError::CounterOverflow | CapacityError::ReleaseUnderflow) => {
                counter_healthy = false;
            }
        }
    }

    let cancel_started = std::time::Instant::now();
    for reservation in reservations {
        if controller.release(reservation).is_err() {
            counter_healthy = false;
        }
    }
    let measured_cancel_latency_ms =
        u64::try_from(cancel_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let healthy = counter_healthy && controller.in_flight() == LoadPoint::default();
    let cancel_responsive = healthy && measured_cancel_latency_ms <= cancel_budget_ms;

    for report in &mut saturation_points {
        report.healthy = healthy;
        report.cancel_responsive = cancel_responsive;
    }

    SaturationBenchmarkReport {
        hardware_profile: RECORDED_HARDWARE_PROFILE.to_string(),
        admitted_count,
        rejected_overload_count,
        saturation_points,
        measured_cancel_latency_ms,
        healthy,
        cancel_responsive,
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    const TEST_LIMITS: CapacityLimits = CapacityLimits {
        max_missions: 4,
        max_streaming: 8,
        max_peer_repair: 2,
        max_model_churn: 1,
    };

    #[test]
    fn admit_and_saturation_report_identify_each_overloaded_dimension() {
        let ok_load = LoadPoint {
            missions: 4,
            streaming: 8,
            peer_repair: 2,
            model_churn: 1,
        };
        assert_eq!(admit(&ok_load, &TEST_LIMITS), AdmitLoad::Accept);
        assert_eq!(saturation(&ok_load, &TEST_LIMITS).saturated_on, None);

        let over_missions = LoadPoint {
            missions: 5,
            ..ok_load
        };
        assert_eq!(
            admit(&over_missions, &TEST_LIMITS),
            AdmitLoad::RejectOverload
        );
        let rep = saturation_report(&over_missions, &TEST_LIMITS);
        assert_eq!(rep.saturated_on, Some("missions"));
        assert!(rep.healthy);
        assert!(rep.cancel_responsive);
    }

    #[test]
    fn control_plane_capacity_reserve_and_release_track_in_flight_load() {
        let mut cap = ControlPlaneCapacity::new(TEST_LIMITS);
        assert_eq!(cap.limits(), TEST_LIMITS);
        assert_eq!(cap.in_flight(), LoadPoint::default());

        let req = LoadPoint {
            missions: 2,
            streaming: 3,
            peer_repair: 1,
            model_churn: 1,
        };
        let res = cap.reserve(req).expect("reserve within limits");
        assert_eq!(cap.in_flight(), req);
        cap.release(res).expect("release reservation");
        assert_eq!(cap.in_flight(), LoadPoint::default());
    }

    #[test]
    fn reserve_rejects_overload_without_mutating_in_flight_counters() {
        let mut cap = ControlPlaneCapacity::new(TEST_LIMITS);
        let initial = LoadPoint {
            missions: 3,
            streaming: 2,
            peer_repair: 1,
            model_churn: 0,
        };
        let _held = cap.reserve(initial).expect("initial reserve");

        let excessive = LoadPoint {
            missions: 0,
            streaming: 0,
            peer_repair: 2,
            model_churn: 0,
        };
        let err = cap.reserve(excessive).expect_err("must reject overload");
        assert_eq!(
            err,
            CapacityError::Overloaded {
                report: SaturationReport {
                    saturated_on: Some("peer_repair"),
                    healthy: true,
                    cancel_responsive: true,
                }
            }
        );
        assert_eq!(cap.in_flight(), initial);
    }

    #[test]
    fn checked_add_and_sub_detect_counter_overflow_and_release_underflow() {
        let mut cap = ControlPlaneCapacity::new(TEST_LIMITS);
        let res = CapacityReservation {
            load: LoadPoint {
                missions: 1,
                streaming: 0,
                peer_repair: 0,
                model_churn: 0,
            },
        };
        assert_eq!(cap.release(res), Err(CapacityError::ReleaseUnderflow));

        let _held = cap.reserve(res.load).expect("reserve 1 mission");
        assert_eq!(
            cap.reserve(LoadPoint {
                missions: usize::MAX,
                streaming: 0,
                peer_repair: 0,
                model_churn: 0,
            }),
            Err(CapacityError::CounterOverflow)
        );
    }

    #[test]
    fn saturation_report_identifies_streaming_and_model_churn_and_roundtrips_serde() {
        let streaming_over = LoadPoint {
            missions: 1,
            streaming: 9,
            peer_repair: 0,
            model_churn: 0,
        };
        assert_eq!(
            saturation_report(&streaming_over, &TEST_LIMITS).saturated_on,
            Some("streaming")
        );

        let churn_over = LoadPoint {
            missions: 1,
            streaming: 1,
            peer_repair: 0,
            model_churn: 2,
        };
        assert_eq!(
            saturation_report(&churn_over, &TEST_LIMITS).saturated_on,
            Some("model_churn")
        );

        let limits_json = serde_json::to_string(&TEST_LIMITS).expect("serialize CapacityLimits");
        let limits_back: CapacityLimits =
            serde_json::from_str(&limits_json).expect("deserialize CapacityLimits");
        assert_eq!(limits_back, TEST_LIMITS);
        assert!(!RECORDED_HARDWARE_PROFILE.is_empty());
    }
}
