//! Graceful Degradation & Fallback Router (Swarm OS Bullet 68)
//!
//! Provides structured graceful degradation for AI inference. If a primary
//! LLM backend (e.g. cloud or high-parameter model) fails or times out,
//! the router automatically falls back to a locally quantized model.

use std::sync::RwLock;

/// Represents an AI inference endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceEndpoint {
    PrimaryCloud(String),
    SecondaryCloud(String),
    LocalQuantized(String),
}

/// The result of an inference operation.
#[derive(Debug, Clone)]
pub enum InferenceResult {
    Success(String),
    Timeout,
    ConnectionError(String),
    RateLimited,
}

/// Manages routing and fallback logic for inference requests.
pub struct FallbackRouter {
    primary: RwLock<InferenceEndpoint>,
    fallback: RwLock<InferenceEndpoint>,
}

impl Default for FallbackRouter {
    fn default() -> Self {
        Self::new(
            InferenceEndpoint::PrimaryCloud(
                "https://api.openai.com/v1/chat/completions".to_string(),
            ),
            InferenceEndpoint::LocalQuantized(
                "localhost:8080/v1/models/llama-quantized".to_string(),
            ),
        )
    }
}

impl FallbackRouter {
    pub fn new(primary: InferenceEndpoint, fallback: InferenceEndpoint) -> Self {
        Self {
            primary: RwLock::new(primary),
            fallback: RwLock::new(fallback),
        }
    }

    /// Sets a new primary endpoint.
    pub fn set_primary(&self, endpoint: InferenceEndpoint) {
        let mut guard = self.primary.write().unwrap_or_else(|e| e.into_inner());
        *guard = endpoint;
    }

    /// Simulates executing an inference request with automatic fallback.
    /// In a real implementation, this would make async HTTP/gRPC calls.
    pub fn execute_with_fallback<F>(&self, mut executor: F) -> Result<String, String>
    where
        F: FnMut(&InferenceEndpoint) -> InferenceResult,
    {
        let primary_ep = self
            .primary
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // Attempt Primary
        match executor(&primary_ep) {
            InferenceResult::Success(response) => return Ok(response),
            InferenceResult::Timeout
            | InferenceResult::ConnectionError(_)
            | InferenceResult::RateLimited => {
                // Primary failed, proceed to fallback
            }
        }

        let fallback_ep = self
            .fallback
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // Attempt Fallback
        match executor(&fallback_ep) {
            InferenceResult::Success(response) => Ok(response),
            err @ InferenceResult::Timeout
            | err @ InferenceResult::ConnectionError(_)
            | err @ InferenceResult::RateLimited => Err(format!(
                "Both primary and fallback endpoints failed. Last error: {:?}",
                err
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graceful_degradation_fallback() {
        let router = FallbackRouter::default();

        // Scenario 1: Primary succeeds
        let res1 = router.execute_with_fallback(|ep| {
            if let InferenceEndpoint::PrimaryCloud(_) = ep {
                InferenceResult::Success("primary response".to_string())
            } else {
                InferenceResult::ConnectionError("Should not reach here".to_string())
            }
        });
        assert_eq!(res1.unwrap(), "primary response");

        // Scenario 2: Primary fails, fallback succeeds
        let res2 = router.execute_with_fallback(|ep| {
            if let InferenceEndpoint::PrimaryCloud(_) = ep {
                InferenceResult::Timeout
            } else {
                InferenceResult::Success("local fallback response".to_string())
            }
        });
        assert_eq!(res2.unwrap(), "local fallback response");

        // Scenario 3: Both fail
        let res3 = router.execute_with_fallback(|_| InferenceResult::RateLimited);
        assert!(res3.is_err());
    }
}
