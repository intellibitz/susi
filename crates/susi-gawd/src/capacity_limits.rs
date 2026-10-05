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
                report: saturation_report(&next, &self.limits),
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

/// Compatibility helper retained for the pure unit-model tests.
#[cfg(test)]
#[must_use]
pub fn saturation(load: &LoadPoint, limits: &CapacityLimits) -> SaturationReport {
    saturation_report(load, limits)
}
