// SUSI Network Guard: API Authentication & Rate Limiting for world-facing
// HTTP surfaces (GMCP HTTP, GEMI REST). RULE 12: Hardware Authority (DoS
// prevention) & general perimeter hardening for a substrate that is
// reachable from the open network, not just localhost.

use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct NetGuard;

impl NetGuard {
    /// Whether an `Authorization: Bearer <token>` header satisfies the
    /// configured `api_auth_token`. If no token is configured (the bundled
    /// default), every request is authorized — auth is opt-in so existing
    /// local/desktop installs keep working unmodified; an operator exposing
    /// susi beyond localhost is expected to set `api_auth_token`.
    pub fn is_authorized(auth_header: Option<&str>) -> bool {
        let token = crate::sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        if token.is_empty() {
            return true;
        }
        auth_header
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|presented| presented == token)
            .unwrap_or(false)
    }
}

/// Fixed-window rate limiter keyed by client IP. Not a precise sliding
/// window — good enough to blunt basic abuse/DoS without pulling in an
/// external crate (Mandate 3: 100% Bloat Rejection).
pub struct RateLimiter {
    buckets: DashMap<IpAddr, (Instant, u32)>,
    // Bounds memory under an attacker spraying requests from many distinct
    // source IPs specifically to grow this map — same "cap size, evict an
    // entry when over capacity" pattern as HighDensityContextStore.
    capacity: usize,
    evictions: AtomicU64,
}

impl RateLimiter {
    const DEFAULT_CAPACITY: usize = 50_000;
    const WINDOW: Duration = Duration::from_secs(60);

    pub fn global() -> &'static RateLimiter {
        static INSTANCE: OnceLock<RateLimiter> = OnceLock::new();
        INSTANCE.get_or_init(|| RateLimiter {
            buckets: DashMap::new(),
            capacity: Self::DEFAULT_CAPACITY,
            evictions: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    fn with_capacity(capacity: usize) -> RateLimiter {
        RateLimiter {
            buckets: DashMap::new(),
            capacity,
            evictions: AtomicU64::new(0),
        }
    }

    /// Returns true if `ip` is still within its per-minute request budget.
    /// A `limit` of 0 disables rate limiting entirely.
    pub fn check(&self, ip: IpAddr, limit: u32) -> bool {
        if limit == 0 {
            return true;
        }

        let now = Instant::now();

        if self.buckets.len() >= self.capacity && !self.buckets.contains_key(&ip) {
            // Self-deadlock hazard: `self.buckets.iter()` is an unnamed
            // temporary, and DashMap's `Iter` holds its current shard's read
            // lock for the `Iter`'s own lifetime (not just the yielded
            // `RefMulti`'s). Using it directly as an `if let` scrutinee
            // extends that temporary's lifetime to the end of the block
            // (Rust's standard "if let" temporary-extension rule), so
            // `remove()` below would try to take a write lock on the same
            // shard whose read lock the still-alive `Iter` temporary is
            // holding. Binding to a `let` first forces the `Iter` (and its
            // lock) to drop at the end of this statement, before `remove()`
            // ever runs.
            let stale_key = self.buckets.iter().next().map(|r| *r.key());
            if let Some(stale_key) = stale_key {
                self.buckets.remove(&stale_key);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut entry = self.buckets.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= Self::WINDOW {
            *entry = (now, 1);
            return true;
        }
        entry.1 += 1;
        entry.1 <= limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_authorized_open_when_no_token_configured() {
        // Bundled default api_auth_token is "", so any header (or none) passes.
        assert!(NetGuard::is_authorized(None));
        assert!(NetGuard::is_authorized(Some("Bearer anything")));
    }

    #[test]
    fn test_rate_limiter_allows_up_to_limit_then_blocks() {
        let limiter = RateLimiter::with_capacity(10);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for i in 0..3 {
            assert!(limiter.check(ip, 3), "request {} within limit should pass", i);
        }
        assert!(!limiter.check(ip, 3), "4th request over limit should be blocked");
    }

    #[test]
    fn test_rate_limiter_zero_limit_disables_check() {
        let limiter = RateLimiter::with_capacity(10);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..1000 {
            assert!(limiter.check(ip, 0));
        }
    }

    #[test]
    fn test_rate_limiter_tracks_ips_independently() {
        let limiter = RateLimiter::with_capacity(10);
        let ip_a: IpAddr = "10.0.0.1".parse().unwrap();
        let ip_b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(limiter.check(ip_a, 1));
        assert!(!limiter.check(ip_a, 1));
        // A different IP has its own independent budget.
        assert!(limiter.check(ip_b, 1));
    }

    #[test]
    fn test_rate_limiter_evicts_when_over_capacity() {
        let limiter = RateLimiter::with_capacity(2);
        let ip_a: IpAddr = "10.0.1.1".parse().unwrap();
        let ip_b: IpAddr = "10.0.1.2".parse().unwrap();
        let ip_c: IpAddr = "10.0.1.3".parse().unwrap();
        assert!(limiter.check(ip_a, 100));
        assert!(limiter.check(ip_b, 100));
        // Map is now at capacity; a third distinct IP must evict rather than
        // grow the map unbounded, and must still be allowed through.
        assert!(limiter.check(ip_c, 100));
        assert!(limiter.buckets.len() <= 2);
    }
}
