//! Speculative decoding routing: local draft, cloud verify.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecRoute {
    pub draft_local: bool,
    pub verify_cloud: bool,
}

/// Use local draft + cloud verify when both are available and task is latency-sensitive.
#[must_use]
pub fn speculative_route(
    has_local_draft: bool,
    has_cloud: bool,
    latency_sensitive: bool,
) -> SpecRoute {
    let on = has_local_draft && has_cloud && latency_sensitive;
    SpecRoute {
        draft_local: on,
        verify_cloud: on,
    }
}

#[cfg(test)]
mod speculative_routing_tests {
    use super::*;

    #[test]
    fn speculative_routing_needs_draft_and_cloud() {
        assert!(speculative_route(true, true, true).draft_local);
        assert!(!speculative_route(true, false, true).verify_cloud);
    }
}
