//! Arbitrate per-key rate and concurrency across everything running.
//!
//! Tracks concurrent operations and rate limits per API key/credential,
//! enforcing global concurrency constraints and per-key rate limits.
//! Admits new operations only when both key-level and global constraints allow.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-key rate and concurrency state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyState {
    /// Current active operations for this key.
    pub concurrent_ops: u32,
    /// Maximum allowed concurrent operations for this key.
    pub max_concurrent: u32,
    /// Requests allowed per time window for this key.
    pub rate_limit: u32,
    /// Requests used in current time window.
    pub requests_used: u32,
}

impl KeyState {
    /// Create a new key state with given limits.
    pub fn new(max_concurrent: u32, rate_limit: u32) -> Self {
        Self {
            concurrent_ops: 0,
            max_concurrent,
            rate_limit,
            requests_used: 0,
        }
    }

    /// Check if a new operation can start.
    pub fn can_admit(&self) -> bool {
        self.concurrent_ops < self.max_concurrent && self.requests_used < self.rate_limit
    }

    /// Admit a new operation (increment both counters).
    pub fn admit(&mut self) {
        self.concurrent_ops += 1;
        self.requests_used += 1;
    }

    /// Complete an operation (decrement concurrent counter).
    pub fn complete(&mut self) {
        if self.concurrent_ops > 0 {
            self.concurrent_ops -= 1;
        }
    }

    /// Reset rate limit counters (called per time window).
    pub fn reset_rate_window(&mut self) {
        self.requests_used = 0;
    }

    /// Check if rate limit is exhausted.
    pub fn is_rate_limited(&self) -> bool {
        self.requests_used >= self.rate_limit
    }

    /// Check if concurrency limit is reached.
    pub fn is_concurrency_limited(&self) -> bool {
        self.concurrent_ops >= self.max_concurrent
    }
}

/// Global arbitration state across all keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateArbitrator {
    /// Per-key state keyed by credential ID.
    keys: HashMap<String, KeyState>,
    /// Global maximum concurrent operations across all keys.
    global_max_concurrent: u32,
    /// Current global concurrent operations.
    global_concurrent: u32,
}

impl RateArbitrator {
    /// Create a new arbitrator with global concurrency limit.
    pub fn new(global_max_concurrent: u32) -> Self {
        Self {
            keys: HashMap::new(),
            global_max_concurrent,
            global_concurrent: 0,
        }
    }

    /// Register or update a key's limits.
    pub fn set_key_limits(
        &mut self,
        key_id: impl Into<String>,
        max_concurrent: u32,
        rate_limit: u32,
    ) {
        self.keys
            .insert(key_id.into(), KeyState::new(max_concurrent, rate_limit));
    }

    /// Check if an operation can be admitted for the given key.
    pub fn can_admit(&self, key_id: &str) -> bool {
        // Check global limit
        if self.global_concurrent >= self.global_max_concurrent {
            return false;
        }

        // Check key-level limits
        self.keys
            .get(key_id)
            .map(|ks| ks.can_admit())
            .unwrap_or(false)
    }

    /// Admit an operation for the given key.
    pub fn admit(&mut self, key_id: &str) -> Result<(), String> {
        if !self.can_admit(key_id) {
            return Err("Cannot admit: global or key limit reached".to_string());
        }

        if let Some(key_state) = self.keys.get_mut(key_id) {
            key_state.admit();
            self.global_concurrent += 1;
            Ok(())
        } else {
            Err(format!("Key {} not registered", key_id))
        }
    }

    /// Complete an operation for the given key.
    pub fn complete(&mut self, key_id: &str) {
        if let Some(key_state) = self.keys.get_mut(key_id) {
            key_state.complete();
            if self.global_concurrent > 0 {
                self.global_concurrent -= 1;
            }
        }
    }

    /// Reset rate limits for all keys (called per time window).
    pub fn reset_rate_windows(&mut self) {
        for key_state in self.keys.values_mut() {
            key_state.reset_rate_window();
        }
    }

    /// Get current global concurrent operation count.
    pub fn global_concurrent(&self) -> u32 {
        self.global_concurrent
    }

    /// Get state for a specific key.
    pub fn key_state(&self, key_id: &str) -> Option<&KeyState> {
        self.keys.get(key_id)
    }

    /// Get overall saturation report.
    pub fn saturation(&self) -> SaturationReport {
        let mut rate_limited_keys = vec![];
        let mut concurrency_limited_keys = vec![];

        for (key_id, state) in &self.keys {
            if state.is_rate_limited() {
                rate_limited_keys.push(key_id.clone());
            }
            if state.is_concurrency_limited() {
                concurrency_limited_keys.push(key_id.clone());
            }
        }

        let global_limited = self.global_concurrent >= self.global_max_concurrent;

        SaturationReport {
            global_limited,
            rate_limited_keys,
            concurrency_limited_keys,
        }
    }
}

/// System-wide saturation status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaturationReport {
    pub global_limited: bool,
    pub rate_limited_keys: Vec<String>,
    pub concurrency_limited_keys: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_rate_arbitration() {
        // Create arbitrator with global limit of 10 concurrent ops
        let mut arb = RateArbitrator::new(10);

        // Register two keys with different limits
        arb.set_key_limits("key_a", 3, 5); // max 3 concurrent, 5 per window
        arb.set_key_limits("key_b", 2, 3); // max 2 concurrent, 3 per window

        // key_a should be able to admit operations up to its limit
        assert!(arb.can_admit("key_a"));
        assert_eq!(arb.admit("key_a"), Ok(()));
        assert!(arb.can_admit("key_a"));
        assert_eq!(arb.admit("key_a"), Ok(()));
        assert!(arb.can_admit("key_a"));
        assert_eq!(arb.admit("key_a"), Ok(()));

        // key_a is at concurrency limit, cannot admit more
        assert!(!arb.can_admit("key_a"));

        // key_b should still be able to admit
        assert!(arb.can_admit("key_b"));
        assert_eq!(arb.admit("key_b"), Ok(()));
        assert_eq!(arb.admit("key_b"), Ok(()));

        // Global concurrent count should match
        assert_eq!(arb.global_concurrent(), 5);

        // Complete an operation for key_a
        arb.complete("key_a");
        assert_eq!(arb.global_concurrent(), 4);

        // Can admit another for key_a now
        assert!(arb.can_admit("key_a"));

        // Rate limit test: exhaust key_a's rate limit
        arb.reset_rate_windows(); // Reset to allow more operations
        arb.set_key_limits("key_c", 10, 2); // max 10 concurrent, 2 per window
        assert_eq!(arb.admit("key_c"), Ok(()));
        assert_eq!(arb.admit("key_c"), Ok(()));

        // key_c is rate limited now
        if let Some(state) = arb.key_state("key_c") {
            assert!(state.is_rate_limited());
        }
    }

    #[test]
    fn rate_arbitration_global_limit() {
        let mut arb = RateArbitrator::new(3); // Global limit of 3

        arb.set_key_limits("key_1", 10, 100);
        arb.set_key_limits("key_2", 10, 100);

        // Can admit up to global limit
        assert_eq!(arb.admit("key_1"), Ok(()));
        assert_eq!(arb.admit("key_1"), Ok(()));
        assert_eq!(arb.admit("key_2"), Ok(()));

        // Now at global limit
        assert!(!arb.can_admit("key_1"));
        assert!(!arb.can_admit("key_2"));

        // Complete one operation
        arb.complete("key_1");
        assert_eq!(arb.global_concurrent(), 2);

        // Can admit one more
        assert!(arb.can_admit("key_2"));
        assert_eq!(arb.admit("key_2"), Ok(()));
    }

    #[test]
    fn rate_arbitration_saturation_report() {
        let mut arb = RateArbitrator::new(10);

        arb.set_key_limits("key_a", 2, 3);
        arb.set_key_limits("key_b", 1, 2);

        // Fill key_a concurrency
        arb.admit("key_a").ok();
        arb.admit("key_a").ok();

        // Exhaust key_a rate limit (one more admission uses up rate window)
        arb.admit("key_a").ok();

        // Fill key_b
        arb.admit("key_b").ok();

        let report = arb.saturation();
        assert!(report.rate_limited_keys.contains(&"key_a".to_string()));
        assert!(report
            .concurrency_limited_keys
            .contains(&"key_a".to_string()));
        assert!(report
            .concurrency_limited_keys
            .contains(&"key_b".to_string()));
    }
}
