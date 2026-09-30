//! Batch API for non-urgent work.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchJob {
    pub id: String,
    pub requests: usize,
    pub deferred: bool,
}

/// Queue non-urgent work as a batch when urgency is low.
#[must_use]
pub fn enqueue_batch(id: &str, requests: usize, urgent: bool) -> BatchJob {
    BatchJob {
        id: id.into(),
        requests,
        deferred: !urgent && requests > 0,
    }
}

#[cfg(test)]
mod batch_api_tests {
    use super::*;

    #[test]
    fn batch_api_defers_non_urgent() {
        assert!(enqueue_batch("b1", 10, false).deferred);
        assert!(!enqueue_batch("b2", 10, true).deferred);
    }
}
