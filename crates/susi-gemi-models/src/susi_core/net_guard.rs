// SUSI Network Guard: API Authentication & Rate Limiting for world-facing
// HTTP surfaces (GMCP HTTP, GEMI REST). Mandate 12: Hardware Authority (DoS
// prevention) & Mandate 40: world-facing surface authentication.
//
// Zero-trust default: after the host seeds `api_auth_token`, every HTTP
// request (including localhost) must present `Authorization: Bearer <token>`.
// Before seeding, non-loopback peers are denied (fail closed on the wire).

use dashmap::DashMap;
use sha2::Digest;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct NetGuard;

/// The `X-Susi-*` request-signature headers a member node attaches to
/// peer MCP calls (`mcp_client` signs every POST when `node.key` is
/// loadable). The v2 signature covers
/// `susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{sha256(body)}`
/// — a passive sniffer captures headers that cannot be re-minted for a
/// different request, the nonce cache makes verbatim replay fail
/// outright, and the body hash closes content substitution by an
/// on-path relay (v1 signatures covering only method+path remain
/// verifiable for pre-body-bound senders).
pub struct SignedRequest<'a> {
    pub node: Option<&'a str>,
    pub ts_secs: Option<u64>,
    pub nonce: Option<&'a str>,
    pub sig: Option<&'a str>,
}

impl SignedRequest<'_> {
    /// Any signature-related header present — the request is a signed
    /// attempt (valid or malformed), distinct from a plain bearer call.
    pub fn present(&self) -> bool {
        self.sig.is_some() || self.node.is_some() || self.nonce.is_some()
    }
}

/// Clock skew tolerated between the sender's timestamp and local time.
const REQ_SKEW_SECS: u64 = 300;

impl NetGuard {
    /// Whether a request satisfies admission for this peer.
    ///
    /// - Valid signed request → authorized on its own (proof of `node.key`
    ///   possession beats any bearer).
    /// - Malformed/invalid signed attempt → falls through to the bearer
    ///   check (an unknown or unbound node still authenticates by
    ///   credential; a *bound* member's bearer is refused anyway below).
    /// - Token configured → bearer required for every peer (zero-trust).
    /// - Token empty (pre-seed) → loopback only; remote peers denied.
    /// - A presented credential also passes when it is the cluster
    ///   peer bearer — derived from the shared `cluster.key`, so every
    ///   member node presents the same value — **unless** the source
    ///   address belongs to a member whose pubkey is bound: once a
    ///   member can prove `node.key` possession, the sniffable shared
    ///   bearer no longer suffices from its registered address.
    ///
    /// `body` is the buffered request bytes — required to verify a v2
    /// (body-bound) signature; callers that cannot buffer pass `None`
    /// and only v1 (method+path) signatures verify for them.
    #[allow(clippy::too_many_arguments)] // one flat auth-context — a struct
                                         // would only rename the parameters
    pub fn is_authorized(
        auth_header: Option<&str>,
        peer: IpAddr,
        signed: &SignedRequest<'_>,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> bool {
        if signed.present() && Self::signed_request_valid(signed, &peer, method, path, body) {
            return true;
        }
        let token = crate::susi_config::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        let presented = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        match presented {
            Some(p) => {
                if !token.is_empty() && Self::constant_time_eq(p.as_bytes(), token.as_bytes()) {
                    return true;
                }
                crate::susi_config::cluster_key::peer_bearer().is_some_and(|pb| {
                    Self::constant_time_eq(p.as_bytes(), pb.as_bytes())
                        // The peer bearer derives from the shared
                        // cluster.key — a banned member still holds it
                        // and derives a valid credential. Membership
                        // standing is the second half of the check:
                        // refuse bearer calls arriving from a banned
                        // member's registered address (TCP sources
                        // can't be spoofed; a banned node re-homing to
                        // a fresh address is the documented residual —
                        // full revocation needs cluster re-key).
                        && !Self::peer_address_banned(&peer)
                        // A member with a bound node key must prove it:
                        // the bearer is sniffable plaintext and this peer
                        // is capable of signing, so bearer-only calls from
                        // its registered address are refused.
                        && !Self::peer_key_bound(&peer)
                })
            }
            None => token.is_empty() && peer.is_loopback(),
        }
    }

    /// Verify a signed peer request: the claimed node must be a roster
    /// member with a bound pubkey, the source IP must be that member's
    /// registered address, the timestamp must be inside the skew window,
    /// the nonce must be fresh, and the Ed25519 signature must cover the
    /// canonical request string — v2 (body-bound) checked first, v1
    /// (method+path only) accepted for senders that predate body
    /// binding.
    fn signed_request_valid(
        signed: &SignedRequest<'_>,
        peer: &IpAddr,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> bool {
        let (Some(node), Some(ts), Some(nonce), Some(sig)) =
            (signed.node, signed.ts_secs, signed.nonce, signed.sig)
        else {
            return false;
        };
        let Some((pubkey, member_ip)) = Self::bound_member(node) else {
            return false;
        };
        if member_ip != *peer {
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.abs_diff(ts) > REQ_SKEW_SECS {
            return false;
        }
        if !Self::record_nonce(node, nonce) {
            return false;
        }
        if let Some(bytes) = body {
            let hash = hex::encode(sha2::Sha256::digest(bytes));
            let v2 = format!("susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{hash}");
            if crate::susi_config::cluster_key::member_verify(&pubkey, &v2, sig) {
                return true;
            }
        }
        let v1 = format!("susi-peer-req-v1:{node}:{ts}:{nonce}:{method}:{path}");
        crate::susi_config::cluster_key::member_verify(&pubkey, &v1, sig)
    }

    /// The `(pubkey, registered_ip)` of a roster member whose node key is
    /// bound — the roster binding is what makes a member-signed request
    /// meaningful (the pubkey was committed by `member_add`, not
    /// self-asserted).
    fn bound_member(node: &str) -> Option<(String, IpAddr)> {
        let path = crate::susi_paths::SusiDirs::config_dir().join("peers.json");
        let text = std::fs::read_to_string(path).ok()?;
        let peers = serde_json::from_str::<Vec<serde_json::Value>>(&text).ok()?;
        peers.iter().find_map(|p| {
            if p.get("node_id").and_then(|v| v.as_str()) != Some(node) {
                return None;
            }
            let pubkey = p.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
            if pubkey.is_empty() {
                return None;
            }
            let ip = p
                .get("address")
                .and_then(|v| v.as_str())
                .and_then(|a| a.split(':').next())
                .and_then(|h| h.parse::<IpAddr>().ok())?;
            Some((pubkey.to_string(), ip))
        })
    }

    /// Whether `ip` is the registered address of a member whose node key
    /// is bound — such members must authenticate by signature, not the
    /// sniffable shared bearer.
    fn peer_key_bound(ip: &IpAddr) -> bool {
        let path = crate::susi_paths::SusiDirs::config_dir().join("peers.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(peers) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return false;
        };
        peers.iter().any(|p| {
            !p.get("pubkey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
                && p.get("address")
                    .and_then(|a| a.as_str())
                    .and_then(|a| a.split(':').next())
                    .and_then(|h| h.parse::<IpAddr>().ok())
                    .is_some_and(|pip| pip == *ip)
        })
    }

    /// First-use insert for `{node}:{nonce}` — false on replay. Bounded:
    /// at capacity an arbitrary entry is evicted (same self-deadlock-safe
    /// pattern as `RateLimiter`); a reused nonce after eviction still
    /// fails because the covering timestamp expired with the skew window.
    fn record_nonce(node: &str, nonce: &str) -> bool {
        static NONCES: OnceLock<DashMap<String, ()>> = OnceLock::new();
        const CAP: usize = 50_000;
        let seen = NONCES.get_or_init(DashMap::new);
        let key = format!("{node}:{nonce}");
        if seen.contains_key(&key) {
            return false;
        }
        if seen.len() >= CAP {
            let stale = seen.iter().next().map(|r| r.key().clone());
            if let Some(stale) = stale {
                seen.remove(&stale);
            }
        }
        seen.insert(key, ()).is_none()
    }

    /// Whether `ip` is the address of a member in `peers_banned.json` —
    /// the standing check for the cluster peer bearer (see above).
    fn peer_address_banned(ip: &IpAddr) -> bool {
        let path = crate::susi_paths::SusiDirs::config_dir().join("peers_banned.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(banned) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return false;
        };
        banned.iter().any(|b| {
            b.get("address")
                .and_then(|a| a.as_str())
                .and_then(|a| a.split(':').next())
                .and_then(|h| h.parse::<IpAddr>().ok())
                .is_some_and(|bip| bip == *ip)
        })
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
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_counter_never_wraps_and_expired_window_resets() {
        let limiter = RateLimiter::with_capacity(10);
        let ip = IpAddr::from([127, 0, 0, 1]);
        limiter.buckets.insert(ip, (Instant::now(), u32::MAX));
        assert!(!limiter.check(ip, u32::MAX));
        limiter
            .buckets
            .insert(ip, (Instant::now() - RateLimiter::WINDOW, u32::MAX));
        assert!(limiter.check(ip, 1));
        assert!(!limiter.check(ip, 1));
    }

    const EMPTY_SIGNED: SignedRequest<'static> = SignedRequest {
        node: None,
        ts_secs: None,
        nonce: None,
        sig: None,
    };

    #[test]
    fn empty_token_allows_loopback_only() {
        // When the live host already seeded a token this test observes the
        // seeded-token path instead — still must not panic.
        let loopback = IpAddr::from([127, 0, 0, 1]);
        let remote = IpAddr::from([8, 8, 8, 8]);
        let token = crate::susi_config::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        if token.is_empty() {
            assert!(NetGuard::is_authorized(
                None,
                loopback,
                &EMPTY_SIGNED,
                "POST",
                "/mcp",
                None
            ));
            assert!(!NetGuard::is_authorized(
                None,
                remote,
                &EMPTY_SIGNED,
                "POST",
                "/mcp",
                None
            ));
        } else {
            assert!(!NetGuard::is_authorized(
                None,
                loopback,
                &EMPTY_SIGNED,
                "POST",
                "/mcp",
                None
            ));
            assert!(NetGuard::is_authorized(
                Some(&format!("Bearer {}", token)),
                loopback,
                &EMPTY_SIGNED,
                "POST",
                "/mcp",
                None
            ));
            assert!(!NetGuard::is_authorized(
                Some("Bearer wrong"),
                remote,
                &EMPTY_SIGNED,
                "POST",
                "/mcp",
                None
            ));
        }
    }

    #[test]
    fn malformed_signed_attempt_falls_through_to_bearer_rules() {
        // Sig headers present but node unknown → not authorized by
        // signature, and no bearer → remote still denied.
        let remote = IpAddr::from([8, 8, 8, 8]);
        let signed = SignedRequest {
            node: Some("susi-node-unknown"),
            ts_secs: Some(0),
            nonce: Some("abcd"),
            sig: Some("00"),
        };
        assert!(!NetGuard::is_authorized(
            None, remote, &signed, "POST", "/mcp", None
        ));
    }

    #[test]
    fn test_constant_time_eq_matches_equal_and_unequal_bytes() {
        assert!(NetGuard::constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!NetGuard::constant_time_eq(
            b"secret-token",
            b"secret-tokeX"
        ));
        assert!(!NetGuard::constant_time_eq(
            b"short",
            b"a-much-longer-value"
        ));
        assert!(NetGuard::constant_time_eq(b"", b""));
    }

    #[test]
    fn test_rate_limiter_allows_up_to_limit_then_blocks() {
        let limiter = RateLimiter::with_capacity(10);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for i in 0..3 {
            assert!(
                limiter.check(ip, 3),
                "request {} within limit should pass",
                i
            );
        }
        assert!(
            !limiter.check(ip, 3),
            "4th request over limit should be blocked"
        );
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
