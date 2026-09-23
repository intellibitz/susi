// SUSI Network Guard: API Authentication & Rate Limiting for world-facing
// HTTP surfaces (GMCP HTTP, GEMI REST). Mandate 12: Hardware Authority (DoS
// prevention) & Mandate 40: world-facing surface authentication.
//
// Zero-trust default: after the host seeds `api_auth_token`, every HTTP
// request (including localhost) must present `Authorization: Bearer <token>`.
// Before seeding, non-loopback peers are denied (fail closed on the wire).

use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct NetGuard;

impl NetGuard {
    /// Whether an `Authorization: Bearer <token>` header satisfies the
    /// configured `api_auth_token` for this peer.
    ///
    /// - Token configured → bearer required for every peer (zero-trust).
    /// - Token empty (pre-seed) → loopback only; remote peers denied.
    pub fn is_authorized(auth_header: Option<&str>, peer: IpAddr) -> bool {
        let token = crate::susi_config::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        if token.is_empty() {
            return peer.is_loopback();
        }
        auth_header
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|presented| Self::constant_time_eq(presented.as_bytes(), token.as_bytes()))
            .unwrap_or(false)
    }

    /// Byte-for-byte comparison that always inspects every byte of both
    /// inputs, so how much of `presented` already matches `expected` can't be
    /// inferred from response timing (CWE-208). A plain `==` short-circuits
    /// on the first mismatching byte, which is enough signal for a remote
    /// attacker to brute-force `api_auth_token` one byte at a time given
    /// sufficiently many timed requests.
    fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
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
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

