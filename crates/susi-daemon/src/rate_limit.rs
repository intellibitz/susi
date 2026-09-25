//! Global Rate Limiter Token Bucket (Swarm OS Bullet 67)
//!
//! Prevents noisy neighbor cells from saturating host bandwidth or syscall throughput
//! by using a token bucket algorithm for each cell.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

/// Configuration for a token bucket.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub max_tokens: u64,
    pub refill_rate_per_sec: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_tokens: 100,         // 100 burst
            refill_rate_per_sec: 10, // 10 tokens per second
        }
    }
}

/// A single cell's token bucket state.
struct TokenBucket {
    tokens: u64,
    last_refill: Instant,
    config: RateLimitConfig,
}

impl TokenBucket {
    fn new(config: RateLimitConfig) -> Self {
        Self {
            tokens: config.max_tokens,
            last_refill: Instant::now(),
            config,
        }
    }

    /// Refills tokens based on elapsed time and attempts to consume one.
    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs();

        if elapsed > 0 {
            let generated = elapsed * self.config.refill_rate_per_sec;
            self.tokens = std::cmp::min(self.config.max_tokens, self.tokens + generated);

            // Only update last_refill if we actually generated full second worth of tokens
            // To be precise we should keep the sub-second remainder, but for a simple OS rate limiter
            // this is sufficient.
            self.last_refill = now;
        }

        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }
}

/// Global manager for rate limiting cells.
pub struct RateLimitManager {
    buckets: RwLock<HashMap<String, TokenBucket>>,
    default_config: RateLimitConfig,
}

impl Default for RateLimitManager {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

impl RateLimitManager {
    pub fn new(default_config: RateLimitConfig) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            default_config,
        }
    }

    /// Explicitly sets a custom rate limit for a specific cell.
    pub fn set_cell_limit(&self, cell_id: &str, config: RateLimitConfig) {
        let mut map = self.buckets.write().unwrap_or_else(|e| e.into_inner());
        map.insert(cell_id.to_string(), TokenBucket::new(config));
    }

    /// Attempts to consume a token for a cell's action.
    /// Returns `true` if allowed, `false` if rate limited.
    pub fn check_allowance(&self, cell_id: &str) -> bool {
        let mut map = self.buckets.write().unwrap_or_else(|e| e.into_inner());
        let bucket = map
            .entry(cell_id.to_string())
            .or_insert_with(|| TokenBucket::new(self.default_config));
        bucket.try_consume()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_token_bucket_consumption() {
        let config = RateLimitConfig {
            max_tokens: 3,
            refill_rate_per_sec: 1,
        };
        let manager = RateLimitManager::new(config);

        let cell_id = "test-cell";

        // Consume all 3 initial tokens
        assert!(manager.check_allowance(cell_id));
        assert!(manager.check_allowance(cell_id));
        assert!(manager.check_allowance(cell_id));

        // 4th should be denied (bucket is empty)
        assert!(!manager.check_allowance(cell_id));

        // Wait a second for refill
        std::thread::sleep(Duration::from_secs(1));

        // Now we should have 1 token
        assert!(manager.check_allowance(cell_id));

        // 2nd should fail again
        assert!(!manager.check_allowance(cell_id));
    }
}
