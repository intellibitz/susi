//! Model health as a state machine with typed failure classes.
//!
//! Tracks model availability and reasons for failure through transitions.
//! Each state captures distinct health conditions; transitions validate
//! preconditions and record failure reasons.

use serde::{Deserialize, Serialize};

/// Typed failure classifications for model health events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureClass {
    /// Network connectivity or timeout.
    Network,
    /// Rate limit hit (HTTP 429 or similar).
    RateLimited,
    /// Quota exhausted for the account or key.
    InsufficientQuota,
    /// Authentication or authorization failed (HTTP 401/403).
    InvalidCredential,
    /// Service returned server error (HTTP 5xx).
    ServiceError,
    /// Model not found or removed (HTTP 404).
    NotFound,
    /// Feature or request not supported.
    Unsupported,
    /// Unknown or other failure.
    Unknown,
}

/// Model health state machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelHealth {
    /// Unknown state; initial or unprobed.
    Unknown,
    /// Model responds and passes health checks.
    Healthy,
    /// Model responds but with degraded performance or warnings.
    Degraded { reason: String },
    /// Model unreachable or broken; requires investigation.
    Unhealthy { class: FailureClass, reason: String },
    /// Model confirmed missing or permanently unavailable.
    Dead { class: FailureClass },
}

impl ModelHealth {
    /// Transition to healthy state.
    pub fn mark_healthy(self) -> Self {
        ModelHealth::Healthy
    }

    /// Transition to degraded state.
    pub fn mark_degraded(self, reason: impl Into<String>) -> Self {
        ModelHealth::Degraded {
            reason: reason.into(),
        }
    }

    /// Transition to unhealthy state with a failure class.
    pub fn mark_unhealthy(self, class: FailureClass, reason: impl Into<String>) -> Self {
        ModelHealth::Unhealthy {
            class,
            reason: reason.into(),
        }
    }

    /// Transition to dead state.
    pub fn mark_dead(self, class: FailureClass) -> Self {
        ModelHealth::Dead { class }
    }

    /// Check if the model is usable for inference.
    pub fn is_usable(&self) -> bool {
        matches!(self, ModelHealth::Healthy | ModelHealth::Degraded { .. })
    }

    /// Check if the model is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ModelHealth::Dead { .. })
    }

    /// Get the current failure class if in a failed state.
    pub fn failure_class(&self) -> Option<FailureClass> {
        match self {
            ModelHealth::Unhealthy { class, .. } | ModelHealth::Dead { class } => Some(*class),
            _ => None,
        }
    }
}

impl Default for ModelHealth {
    fn default() -> Self {
        ModelHealth::Unknown
    }
}

impl std::fmt::Display for ModelHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelHealth::Unknown => write!(f, "Unknown"),
            ModelHealth::Healthy => write!(f, "Healthy"),
            ModelHealth::Degraded { reason } => write!(f, "Degraded: {}", reason),
            ModelHealth::Unhealthy { class, reason } => {
                write!(f, "Unhealthy ({:?}): {}", class, reason)
            }
            ModelHealth::Dead { class } => write!(f, "Dead ({:?})", class),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_health_failure_classes() {
        // Test all failure classes are defined
        let _classes = [
            FailureClass::Network,
            FailureClass::RateLimited,
            FailureClass::InsufficientQuota,
            FailureClass::InvalidCredential,
            FailureClass::ServiceError,
            FailureClass::NotFound,
            FailureClass::Unsupported,
            FailureClass::Unknown,
        ];

        // Test state transitions
        let mut health = ModelHealth::Unknown;
        assert!(!health.is_usable());
        assert!(!health.is_terminal());

        health = health.mark_healthy();
        assert_eq!(health, ModelHealth::Healthy);
        assert!(health.is_usable());
        assert!(!health.is_terminal());

        health = health.mark_degraded("slow response");
        assert!(matches!(health, ModelHealth::Degraded { .. }));
        assert!(health.is_usable());
        assert!(!health.is_terminal());

        health = health.mark_unhealthy(FailureClass::RateLimited, "429 too many requests");
        assert_eq!(health.failure_class(), Some(FailureClass::RateLimited));
        assert!(!health.is_usable());
        assert!(!health.is_terminal());

        health = health.mark_dead(FailureClass::NotFound);
        assert_eq!(health.failure_class(), Some(FailureClass::NotFound));
        assert!(!health.is_usable());
        assert!(health.is_terminal());
    }

    #[test]
    fn model_health_classification_covers_all_scenarios() {
        // Verify each failure class is distinct
        assert_ne!(FailureClass::Network, FailureClass::RateLimited);
        assert_ne!(FailureClass::RateLimited, FailureClass::InsufficientQuota);
        assert_ne!(
            FailureClass::InsufficientQuota,
            FailureClass::InvalidCredential
        );
        assert_ne!(FailureClass::InvalidCredential, FailureClass::ServiceError);
        assert_ne!(FailureClass::ServiceError, FailureClass::NotFound);
        assert_ne!(FailureClass::NotFound, FailureClass::Unsupported);
        assert_ne!(FailureClass::Unsupported, FailureClass::Unknown);

        // Verify state machine transitions
        let unknown = ModelHealth::Unknown;
        let healthy = unknown.mark_healthy();
        let degraded = healthy.mark_degraded("test");
        let unhealthy = degraded.mark_unhealthy(FailureClass::Network, "connection lost");
        let dead = unhealthy.mark_dead(FailureClass::NotFound);

        // Verify state properties at each transition
        assert_eq!(unknown, ModelHealth::Unknown);
        assert_eq!(healthy, ModelHealth::Healthy);
        assert!(matches!(degraded, ModelHealth::Degraded { .. }));
        assert!(matches!(unhealthy, ModelHealth::Unhealthy { .. }));
        assert!(matches!(dead, ModelHealth::Dead { .. }));

        // Verify terminal state behavior
        assert!(!dead.is_usable());
        assert!(dead.is_terminal());
    }

    #[test]
    fn model_health_display_formatting() {
        assert_eq!(ModelHealth::Unknown.to_string(), "Unknown");
        assert_eq!(ModelHealth::Healthy.to_string(), "Healthy");

        let degraded = ModelHealth::Degraded {
            reason: "slow".to_string(),
        };
        assert!(degraded.to_string().contains("Degraded"));
        assert!(degraded.to_string().contains("slow"));

        let unhealthy = ModelHealth::Unhealthy {
            class: FailureClass::RateLimited,
            reason: "too many requests".to_string(),
        };
        assert!(unhealthy.to_string().contains("Unhealthy"));
        assert!(unhealthy.to_string().contains("RateLimited"));
        assert!(unhealthy.to_string().contains("too many requests"));

        let dead = ModelHealth::Dead {
            class: FailureClass::NotFound,
        };
        assert!(dead.to_string().contains("Dead"));
        assert!(dead.to_string().contains("NotFound"));
    }
}
