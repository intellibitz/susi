use std::collections::HashMap;
// SUSI Network Guard: API Authentication & Rate Limiting for world-facing
// HTTP surfaces (GMCP HTTP, GEMI REST). Mandate 12: Hardware Authority (DoS
// prevention) & Mandate 40: world-facing surface authentication.
//
// Zero-trust default: after the host seeds `api_auth_token`, every HTTP
// request (including localhost) must present `Authorization: Bearer <token>`.
// Before seeding, non-loopback peers are denied (fail closed on the wire).

use sha2::Digest;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::sync::RwLock;
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

    /// Authorize a request that arrived inside a sealed peer-channel
    /// session (`susi_config::peer_channel_link`). The session's handshake
    /// proved the cluster key and the caller's node key, so the identity is
    /// the credential and the source address plays no part (behind NAT it
    /// is a router's). Standing is read again per request, so a member
    /// banned after the session opened is refused at its next request:
    ///
    /// - this node's own key, or a member bound to that node id: authorized;
    /// - an identity not bound in this roster (it proved `cluster.key`, the
    ///   standing a cluster peer-bearer caller has): authorized, as the
    ///   shared bearer would be — the membership flows (`peers add`,
    ///   endorsements) run on it;
    /// - banned, or claiming a bound member's node id or key under another
    ///   identity: refused;
    /// - member-signature headers naming a different node than the session
    ///   proved: refused.
    pub fn channel_authorized(
        peer: &crate::susi_config::peer_channel_link::ChannelPeer,
        signed: &SignedRequest<'_>,
    ) -> bool {
        if signed.present() && signed.node != Some(peer.identity.node_id.as_str()) {
            return false;
        }
        crate::susi_config::peer_channel_link::classify(&peer.identity).is_ok()
    }

    /// Verify a signed peer request: the claimed node must be a roster
    /// member with a bound pubkey, the timestamp must be inside the skew window,
    /// the nonce must be fresh, and the Ed25519 signature must cover the
    /// canonical request string — v2 (body-bound) checked first, v1
    /// (method+path only) accepted for senders that predate body
    /// binding.
    fn signed_request_valid(
        signed: &SignedRequest<'_>,
        _peer: &IpAddr,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> bool {
        let (Some(node), Some(ts), Some(nonce), Some(sig)) =
            (signed.node, signed.ts_secs, signed.nonce, signed.sig)
        else {
            return false;
        };
        // Identity-keyed: the roster binds the node id to its key, and the
        // key verifies the request; the address the request came from is a
        // routing fact, not part of the identity (a member behind NAT, a
        // changed public address). Replay is closed by the nonce record and
        // the timestamp window, substitution by the body hash.
        let Some(pubkey) = Self::bound_member(node) else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.abs_diff(ts) > REQ_SKEW_SECS {
            return false;
        }
        let sig_ok = match body {
            Some(bytes) => {
                let hash = hex::encode(sha2::Sha256::digest(bytes));
                let v2 = format!("susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{hash}");
                crate::susi_config::cluster_key::member_verify(&pubkey, &v2, sig) || {
                    let v1 = format!("susi-peer-req-v1:{node}:{ts}:{nonce}:{method}:{path}");
                    crate::susi_config::cluster_key::member_verify(&pubkey, &v1, sig)
                }
            }
            None => {
                let v1 = format!("susi-peer-req-v1:{node}:{ts}:{nonce}:{method}:{path}");
                crate::susi_config::cluster_key::member_verify(&pubkey, &v1, sig)
            }
        };
        // The nonce is consumed only after the signature proves a real
        // member sent this — otherwise unauthenticated garbage headers
        // would burn nonce slots (and disk writes) at will.
        sig_ok && Self::record_nonce(node, nonce)
    }

    /// The pubkey of a roster member whose node key is bound — the roster
    /// binding is what makes a member-signed request meaningful (the pubkey
    /// was committed by `member_add`, not self-asserted). A banned member's
    /// signature authorizes nothing.
    fn bound_member(node: &str) -> Option<String> {
        let pubkey = crate::susi_config::cluster_key::bound_pubkey_for_node(node)?;
        let banned = crate::susi_config::cluster_key::member_banned(|b| {
            b.get("node_id").and_then(|v| v.as_str()) == Some(node)
        });
        (!banned).then_some(pubkey)
    }

    /// Whether `ip` is the registered address of a member whose node key
    /// is bound — such members must authenticate by signature, not the
    /// sniffable shared bearer.
    fn peer_key_bound(ip: &IpAddr) -> bool {
        let peers = crate::susi_config::cluster_key::config_json_rows("peers.json");
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

    /// First-use insert for `{node}:{nonce}` — false on replay. Backed
    /// by a JSONL file in the config dir so a replayed request also
    /// fails across process restarts and sibling servers on the same
    /// node (each process previously kept a private in-memory set, so a
    /// captured request inside the skew window replayed cleanly against
    /// a fresh process or a different port). Mutations serialize
    /// through `FileLock`; a wedged lock fails closed.
    fn record_nonce(node: &str, nonce: &str) -> bool {
        static STORE: OnceLock<std::sync::Mutex<NonceFile>> = OnceLock::new();
        let dir = susi_paths::SusiDirs::config_dir();
        let Some(_guard) = crate::susi_core::commit_log::FileLock::acquire(&dir, "seen_nonces")
        else {
            return false;
        };
        let store = STORE.get_or_init(|| {
            let mut f = NonceFile::open(dir.join("seen_nonces.jsonl"));
            f.refresh();
            std::sync::Mutex::new(f)
        });
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(&format!("{node}:{nonce}"))
    }

    /// Whether `ip` is the address of a member in `peers_banned.json` —
    /// the standing check for the cluster peer bearer (see above).
    fn peer_address_banned(ip: &IpAddr) -> bool {
        // Fails closed on a damaged ban list (see `member_banned`).
        crate::susi_config::cluster_key::member_banned(|b| {
            b.get("address")
                .and_then(|a| a.as_str())
                .and_then(|a| a.split(':').next())
                .and_then(|h| h.parse::<IpAddr>().ok())
                .is_some_and(|bip| bip == *ip)
        })
    }

    /// Whether the request comes from this host's loopback interface and
    /// presents this host's own API token: the local CLI talking to its
    /// own daemon. Such a caller already holds the credential that
    /// authorizes every tool, so metering it guards nothing; peer-bearer,
    /// signed and remote calls are never local-host callers. Only the TCP
    /// peer counts: a reverse proxy on this host would make its clients
    /// look local, so token holders behind one are not metered either.
    pub fn is_local_host_bearer(auth_header: Option<&str>, peer: IpAddr) -> bool {
        if !peer.to_canonical().is_loopback() {
            return false;
        }
        let token = crate::susi_config::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        let presented = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        !token.is_empty()
            && presented.is_some_and(|p| Self::constant_time_eq(p.as_bytes(), token.as_bytes()))
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

/// Append-only nonce ledger (`{node}:{nonce}:{recorded_at}` per line)
/// behind an incremental offset, so `record` costs O(new bytes) per
/// call. Callers hold `FileLock` around `record`, which folds any lines
/// sibling processes appended since this process's last pass — the
/// cross-process dedup an in-memory set cannot give.
struct NonceFile {
    path: std::path::PathBuf,
    seen: std::collections::HashMap<String, u64>,
    offset: u64,
}

impl NonceFile {
    const CAP: usize = 50_000;

    fn open(path: std::path::PathBuf) -> Self {
        Self {
            path,
            seen: std::collections::HashMap::new(),
            offset: 0,
        }
    }

    /// Fold complete lines appended since `offset`; a torn tail line
    /// (crash mid-write) waits for the next pass or gets compacted away.
    fn refresh(&mut self) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return;
        };
        let Ok(meta) = f.metadata() else { return };
        if meta.len() < self.offset {
            // Compacted by a sibling — re-read from the top.
            self.offset = 0;
            self.seen.clear();
        }
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let complete = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        for line in String::from_utf8_lossy(&buf[..complete]).lines() {
            if let Some((key, ts)) = line.rsplit_once(':') {
                if let Ok(t) = ts.parse::<u64>() {
                    self.seen.insert(key.to_string(), t);
                }
            }
        }
        self.offset += complete as u64;
    }

    fn record(&mut self, key: &str) -> bool {
        use std::io::Write;
        self.refresh();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Entries older than twice the skew window can never validate
        // again (their covering timestamp expired), so they stop
        // counting against dedup and get compacted away.
        self.seen
            .retain(|_, t| now.saturating_sub(*t) < REQ_SKEW_SECS * 2);
        if self.seen.contains_key(key) {
            return false;
        }
        self.seen.insert(key.to_string(), now);
        let line = format!("{key}:{now}\n");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            if f.write_all(line.as_bytes()).is_ok() {
                self.offset += line.len() as u64;
            }
        }
        if self.seen.len() > Self::CAP {
            self.compact();
        }
        true
    }

    /// Rewrite the ledger with only live entries — callers already hold
    /// `FileLock`, so the swap is atomic w.r.t. sibling processes.
    fn compact(&mut self) {
        let mut body = String::with_capacity(self.seen.len() * 48);
        for (k, t) in &self.seen {
            body.push_str(&format!("{k}:{t}\n"));
        }
        if crate::susi_config::atomic_write_bytes(&self.path, body.as_bytes()).is_ok() {
            self.offset = body.len() as u64;
        }
    }
}

/// Fixed-window rate limiter keyed by client IP. Not a precise sliding
/// window — good enough to blunt basic abuse/DoS without pulling in an
/// external crate (Mandate 3: 100% Bloat Rejection).
pub struct RateLimiter {
    buckets: RwLock<HashMap<IpAddr, (Instant, u32)>>,
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
            buckets: RwLock::new(HashMap::new()),
            capacity: Self::DEFAULT_CAPACITY,
            evictions: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    fn with_capacity(capacity: usize) -> RateLimiter {
        RateLimiter {
            buckets: RwLock::new(HashMap::new()),
            capacity,
            evictions: AtomicU64::new(0),
        }
    }

    /// The client a bucket belongs to. One IPv6 host is routinely assigned a
    /// whole /64, so keying by the full address let a single client rotate
    /// source addresses past its budget (and flood the map into evicting
    /// other clients' buckets). IPv4-mapped IPv6 counts as its IPv4 address.
    fn bucket_key(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v4) => IpAddr::V4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => {
                    let prefix = u128::from(v6) & (u128::MAX << 64);
                    IpAddr::V6(std::net::Ipv6Addr::from(prefix))
                }
            },
        }
    }

    /// [`RateLimiter::check`] for an already authorized request, except
    /// that the local host's own bearer calls over loopback
    /// ([`NetGuard::is_local_host_bearer`]) are not metered: a thin CLI
    /// makes several requests per command and must not hit 429 on its own
    /// daemon. They do not consume budget either.
    pub fn check_unless_local_host(
        &self,
        auth_header: Option<&str>,
        peer: IpAddr,
        limit: u32,
    ) -> bool {
        NetGuard::is_local_host_bearer(auth_header, peer) || self.check(peer, limit)
    }

    /// Returns true if `ip` is still within its per-minute request budget.
    /// A `limit` of 0 disables rate limiting entirely.
    pub fn check(&self, ip: IpAddr, limit: u32) -> bool {
        if limit == 0 {
            return true;
        }
        let ip = Self::bucket_key(ip);

        let now = Instant::now();

        if self.buckets.read().unwrap_or_else(|e| e.into_inner()).len() >= self.capacity
            && !self
                .buckets
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&ip)
        {
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
            let stale_key = self
                .buckets
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .next()
                .map(|(k, _)| *k);
            if let Some(stale_key) = stale_key {
                self.buckets
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&stale_key);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut binding = self.buckets.write().unwrap_or_else(|e| e.into_inner());
        let entry = binding.entry(ip).or_insert((now, 0));
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
        limiter
            .buckets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ip, (Instant::now(), u32::MAX));
        assert!(!limiter.check(ip, u32::MAX));
        limiter
            .buckets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ip, (Instant::now() - RateLimiter::WINDOW, u32::MAX));
        assert!(limiter.check(ip, 1));
        assert!(!limiter.check(ip, 1));
    }

    #[test]
    fn nonce_file_dedups_across_process_lifetimes() {
        let dir = std::env::temp_dir().join(format!("susi-nonce-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seen_nonces.jsonl");
        {
            let mut f = NonceFile::open(path.clone());
            assert!(f.record("node-a:n1"));
            assert!(!f.record("node-a:n1"));
            assert!(f.record("node-a:n2"));
        }
        // A fresh instance (process restart) folds the file — the same
        // nonce is still refused, a new one still passes.
        {
            let mut f = NonceFile::open(path.clone());
            assert!(!f.record("node-a:n1"));
            assert!(f.record("node-a:n3"));
        }
        // A sibling's line appended *between* this process's calls is
        // folded by the incremental refresh inside the next record.
        {
            use std::io::Write;
            let mut f2 = NonceFile::open(path.clone());
            assert!(f2.record("node-b:x0")); // advances f2's offset
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "node-b:x1:{now}").unwrap();
            assert!(!f2.record("node-b:x1"));
        }
        let _ = std::fs::remove_dir_all(&dir);
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
        // seeded-token path instead — still must not panic. The lock keeps
        // a sibling test's private SUSI_HOME from changing the token between
        // the read below and the checks.
        let _lock = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

    /// Removes a private test home when dropped, also when a test fails.
    struct TempHome(std::path::PathBuf);

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A private host home holding `api_token` and a roster row that binds
    /// a node key to 127.0.0.1 (the host-only fallback a legacy row hits).
    /// Fields drop in order: the env is restored, the home removed, then
    /// the lock released.
    fn local_host_with_bound_loopback_row() -> (
        susi_paths::test_env::EnvGuard,
        TempHome,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let lock = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-net-guard-local-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        let dir = susi_paths::SusiDirs::config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("api_token"), "tok-local\n").unwrap();
        std::fs::write(
            dir.join("peers.json"),
            r#"[{"node_id":"loopback-member","address":"127.0.0.1","pubkey":"3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"}]"#,
        )
        .unwrap();
        (env, TempHome(home), lock)
    }

    #[test]
    fn local_host_bearer_over_loopback_is_not_metered() {
        let _home = local_host_with_bound_loopback_row();
        let limiter = RateLimiter::with_capacity(10);
        let token = Some("Bearer tok-local");
        for peer in [
            IpAddr::from([127, 0, 0, 1]),
            "::1".parse().unwrap(),
            "::ffff:127.0.0.1".parse().unwrap(),
        ] {
            assert!(NetGuard::is_local_host_bearer(token, peer), "{peer}");
            for _ in 0..200 {
                assert!(limiter.check_unless_local_host(token, peer, 2), "{peer}");
            }
        }
        // Exempt calls spend no budget.
        assert!(limiter.buckets.read().unwrap().is_empty());

        // Everyone else stays metered: the token from a remote address, a
        // wrong or missing bearer from loopback.
        let remote = IpAddr::from([192, 0, 2, 7]);
        let loopback = IpAddr::from([127, 0, 0, 1]);
        for (auth, peer) in [
            (token, remote),
            (Some("Bearer wrong"), loopback),
            (None, loopback),
        ] {
            let limiter = RateLimiter::with_capacity(10);
            assert!(!NetGuard::is_local_host_bearer(auth, peer));
            assert!(limiter.check_unless_local_host(auth, peer, 2));
            assert!(limiter.check_unless_local_host(auth, peer, 2));
            assert!(
                !limiter.check_unless_local_host(auth, peer, 2),
                "{auth:?} {peer}"
            );
        }
    }

    /// The thin client never signs or seals. Its plaintext host-bearer call
    /// from loopback is authorized even when a roster row binds a node key
    /// to 127.0.0.1: the bound-member rule refuses only the shared peer
    /// bearer, never the host's own token.
    #[test]
    fn plaintext_loopback_host_bearer_is_authorized_despite_a_bound_loopback_row() {
        let _home = local_host_with_bound_loopback_row();
        let loopback = IpAddr::from([127, 0, 0, 1]);
        assert!(NetGuard::peer_key_bound(&loopback));
        assert!(NetGuard::is_authorized(
            Some("Bearer tok-local"),
            loopback,
            &EMPTY_SIGNED,
            "POST",
            "/mcp",
            Some(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
        ));
        assert!(!NetGuard::is_authorized(
            Some("Bearer wrong"),
            loopback,
            &EMPTY_SIGNED,
            "POST",
            "/mcp",
            None,
        ));
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
    fn ipv6_clients_share_a_bucket_per_64_and_mapped_v4_is_v4() {
        let limiter = RateLimiter::with_capacity(100);
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert!(limiter.check(a, 2));
        assert!(limiter.check(b, 2));
        assert!(!limiter.check(a, 2), "same /64 shares one budget");
        assert!(limiter.check(other, 2), "a different /64 has its own");
        let v4: IpAddr = "192.0.2.7".parse().unwrap();
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert!(limiter.check(v4, 1));
        assert!(!limiter.check(mapped, 1));
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
        assert!(limiter.buckets.read().unwrap().len() <= 2);
    }
}
