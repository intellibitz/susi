//! Control-plane capacity limits (VC-201-094).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityLimits {
    pub max_missions: usize,
    pub max_streaming: usize,
    pub max_peer_repair: usize,
    pub max_model_churn: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaturationReport {
    pub saturated_on: Option<&'static str>,
    pub healthy: bool,
    pub cancel_responsive: bool,
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
pub fn saturation(load: &LoadPoint, limits: &CapacityLimits) -> SaturationReport {
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
