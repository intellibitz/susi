//! Swarm-wide Metrics Aggregator (Swarm OS Bullet 97)
//!
//! Aggregates cluster health metrics globally.

use std::sync::atomic::{AtomicU64, Ordering};

pub struct GlobalMetrics {
    total_inferences: AtomicU64,
    total_tokens: AtomicU64,
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalMetrics {
    pub fn new() -> Self {
        Self {
            total_inferences: AtomicU64::new(0),
            total_tokens: AtomicU64::new(0),
        }
    }

    pub fn record_inference(&self, tokens: u64) {
        self.total_inferences.fetch_add(1, Ordering::Relaxed);
        self.total_tokens.fetch_add(tokens, Ordering::Relaxed);
    }
}
