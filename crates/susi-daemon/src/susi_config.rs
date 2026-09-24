//! Vendored `susi-config` surface + IPC client for the standalone
//! `susi-config` service (`127.0.0.1:18082`, override via `SUSI_CONFIG_PORT`).
//!
//! Decoupled crates own the full config contract locally; only the *global*
//! `~/.susi/config.json` read/write path prefers the service so a running
//! substrate stays the canonical writer. When the service is unreachable —
//! or explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) is set, e.g. in
//! tests with a swapped `HOME` — every call resolves against the local files
//! exactly as the source crate did, so config access never hard-fails on
//! service health. `cluster_key` signing/verification is deliberately local
//! only: exposing HMAC over `cluster.key` (0600) as an unauthenticated
//! localhost endpoint would let any process mint signed cluster messages.
//!
//! Keep this file identical across the workspace.

// This file is vendored byte-identical into crates on different editions;
// `collapsible_if` only fires under edition 2024, where `if let … && …`
// let-chains became stable, but the pre-2021-compatible nested form it
// flags is still required by the edition-2021 consumers. Collapsing here
// would break those crates, so the lint is waived file-wide rather than
// at each of the shared source's sites.
#![allow(clippy::collapsible_if)]

/// IPC client for the standalone `susi-config` service. Only the global
/// config path is routed here; per-directory loads and `cluster_key` stay
/// local by construction.
mod service {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::time::Duration;

    use crate::susi_config::SusiConfig;

    const DEFAULT_PORT: u16 = 18082;
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn addr() -> SocketAddr {
        let port = std::env::var("SUSI_CONFIG_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// Explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) beats the
    /// service — a swapped HOME in tests must not read or write the host
    /// substrate's real `config.json`. Same rule as the vendored
    /// `susi_paths` client.
    fn local_override() -> bool {
        [
            "SUSI_XDG",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
        ]
        .iter()
        .any(|v| std::env::var_os(v).is_some())
    }

    /// Round-trips one HTTP/1.0 request; `None` on any transport failure.
    fn request(req: &str) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&addr(), TIMEOUT).ok()?;
        let _ = stream.set_read_timeout(Some(TIMEOUT));
        let _ = stream.set_write_timeout(Some(TIMEOUT));
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }

    /// Response body when the status line is a 2xx, else `None`.
    fn body(response: &str) -> Option<&str> {
        let status_ok = response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2");
        if !status_ok {
            return None;
        }
        response.split("\r\n\r\n").nth(1)
    }

    /// `GET /config` — healed global config from the running substrate.
    pub fn get_global() -> Option<SusiConfig> {
        if local_override() {
            return None;
        }
        let resp = request("GET /config HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `POST /config` — route a global-dir save through the substrate when
    /// it is up; `false` tells the caller to fall back to the local atomic
    /// write (identical bytes, same file).
    pub fn save_global(cfg: &SusiConfig) -> bool {
        if local_override() {
            return false;
        }
        let Ok(payload) = serde_json::to_string(cfg) else {
            return false;
        };
        let req = format!(
            "POST /config HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        request(&req)
            .is_some_and(|resp| resp.starts_with("HTTP/1.1 2") || resp.starts_with("HTTP/1.0 2"))
    }
}

pub mod cluster_key {
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::PathBuf;

    const BLOCK: usize = 64;
    const PROTO: &str = "susi-peer-v1";

    /// Path to the shared cluster membership key.
    pub fn cluster_key_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("cluster.key")
    }

    /// Load the cluster key, creating a fresh 32-byte key (0600, hex-encoded) on
    /// first use. Returns `None` when the file exists but is malformed, or cannot
    /// be created — callers must treat that as unsigned mode (peers can never be
    /// promoted to `Explicit`).
    pub fn cluster_key() -> Option<[u8; 32]> {
        let path = cluster_key_path();
        if let Ok(text) = fs::read_to_string(&path) {
            let bytes = hex::decode(text.trim()).ok()?;
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Some(key);
            }
            // Present but malformed: do not silently rotate — a fresh key would
            // cut this node off from the existing cluster without telling anyone.
            eprintln!(
                "[cluster.key] {} is malformed (expected 64 hex chars); refusing to auto-rotate",
                path.display()
            );
            return None;
        }
        let dir = crate::susi_paths::SusiDirs::config_dir();
        let _ = fs::create_dir_all(&dir);
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).ok()?;
        let encoded = hex::encode(raw);
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            // Create with 0600 atomically — never write-then-chmod (umask window).
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            if options
                .open(&path)
                .and_then(|mut f| f.write_all(encoded.as_bytes()))
                .is_err()
            {
                // Lost a create race or the fs rejected mode — only accept the
                // key if a peer process actually wrote a valid one.
                return cluster_key();
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&path, &encoded);
        }
        Some(raw)
    }

    /// HMAC-SHA256 over `message` (hand-rolled, same construction as the audit
    /// chain — avoids dragging in a versioned `hmac` crate).
    pub fn hmac_sha256(key: &[u8; 32], message: &[u8]) -> [u8; 32] {
        let mut keyed = [0u8; BLOCK];
        keyed[..32].copy_from_slice(key);
        let mut ipad = [0x36u8; BLOCK];
        let mut opad = [0x5cu8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] ^= keyed[i];
            opad[i] ^= keyed[i];
        }
        let mut inner = Sha256::new();
        inner.update(ipad);
        inner.update(message);
        let inner_hash = inner.finalize();
        let mut outer = Sha256::new();
        outer.update(opad);
        outer.update(inner_hash);
        let out = outer.finalize();
        let mut mac = [0u8; 32];
        mac.copy_from_slice(&out);
        mac
    }

    pub fn hmac_sha256_hex(key: &[u8; 32], message: &[u8]) -> String {
        hex::encode(hmac_sha256(key, message))
    }

    /// Bearer credential for member-to-member MCP calls. The host
    /// `api_token` is a per-node secret — a remote daemon validates
    /// against ITS token and cannot accept ours — so peer calls
    /// (commit pushes, ledger fetches, mission dispatch) present this
    /// cluster.key-derived value instead. Every member holding the
    /// shared key derives the same credential; it is never persisted
    /// and rotates if the cluster key does.
    pub fn peer_bearer() -> Option<String> {
        cluster_key().map(|k| hmac_sha256_hex(&k, b"susi-peer-bearer-v1"))
    }

    /// Staged next-epoch key, delivered by a verified `cluster_rekey`
    /// push and activated only when the committed rekey record lands in
    /// the ledger — a staged key that never gets committed never takes
    /// effect.
    fn staged_key_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("cluster.key.next")
    }

    /// Prior-epoch key retained on activation. Verification accepts it
    /// only for chain-pinned history fills — see
    /// `commit_log::append_to`'s epoch rule — so a revoked key can never
    /// authorize new records but the pre-rotation ledger stays
    /// verifiable for audit.
    fn prev_key_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("cluster.key.prev")
    }

    fn key_bytes_from_file(path: &PathBuf) -> Option<[u8; 32]> {
        let bytes = hex::decode(fs::read_to_string(path).ok()?.trim()).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Some(key)
    }

    /// Write a key file with 0600 mode atomically (tmp + rename).
    fn write_key_file(path: &PathBuf, key: &[u8; 32]) -> bool {
        if let Some(dir) = path.parent() {
            if fs::create_dir_all(dir).is_err() {
                return false;
            }
        }
        let tmp = path.with_extension("tmp");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let written = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .and_then(|mut f| f.write_all(hex::encode(key).as_bytes()))
                .is_ok();
            if !written {
                return false;
            }
        }
        #[cfg(not(unix))]
        {
            if fs::write(&tmp, hex::encode(key)).is_err() {
                return false;
            }
        }
        fs::rename(&tmp, path).is_ok()
    }

    /// A fresh random 32-byte cluster key — the coordinator-side half of
    /// `susi peers rekey`. Not persisted; `stage_key` writes it.
    pub fn generate_key() -> Option<[u8; 32]> {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).ok()?;
        Some(raw)
    }

    /// SHA-256 hex of a key — the value committed in `cluster_rekey`
    /// records. Receivers confirm the pushed key matches the committed
    /// fingerprint before staging, so the ledger — not the transport —
    /// is the authority on which rotation happened.
    pub fn key_fingerprint(key: &[u8; 32]) -> String {
        hex::encode(Sha256::digest(key))
    }

    /// Stage a next-epoch key beside `cluster.key` (0600). The staged
    /// file is inert until a committed `cluster_rekey` record activates
    /// it — writing this file alone changes nothing.
    pub fn stage_key(key: &[u8; 32]) -> bool {
        stage_key_to(key, &staged_key_path())
    }

    /// Test seam: stage to an explicit path.
    pub fn stage_key_to(key: &[u8; 32], path: &PathBuf) -> bool {
        write_key_file(path, key)
    }

    /// Load a staged next-epoch key, if one is waiting for activation.
    pub fn staged_key() -> Option<[u8; 32]> {
        key_bytes_from_file(&staged_key_path())
    }

    /// The prior-epoch key, retained after activation for verifying
    /// pre-rotation history. `None` when no rotation has happened.
    pub fn prev_key() -> Option<[u8; 32]> {
        key_bytes_from_file(&prev_key_path())
    }

    /// Activate the staged key iff its fingerprint matches `fingerprint`
    /// — the committed rekey record's value. On success the current key
    /// rotates to `cluster.key.prev` and the staged key becomes current.
    /// Returns false when no staged key exists or the fingerprint does
    /// not match: applying a rekey record can never clear the key or
    /// activate an unexpected one.
    pub fn activate_staged_key(fingerprint: &str) -> bool {
        activate_staged_key_at(fingerprint, &crate::susi_paths::SusiDirs::config_dir())
    }

    /// Test seam: activate against an explicit directory.
    pub fn activate_staged_key_at(fingerprint: &str, dir: &std::path::Path) -> bool {
        let staged_path = dir.join("cluster.key.next");
        let Some(staged) = key_bytes_from_file(&staged_path) else {
            return false;
        };
        if key_fingerprint(&staged) != fingerprint {
            return false;
        }
        let current_path = dir.join("cluster.key");
        let prev_path = dir.join("cluster.key.prev");
        if let Some(current) = key_bytes_from_file(&current_path) {
            if !write_key_file(&prev_path, &current) {
                return false;
            }
        }
        // rename, not copy — the staged key must not linger beside the
        // active one after rotation.
        fs::rename(&staged_path, &current_path).is_ok()
    }

    /// Fresh random nonce for one handshake exchange (128-bit).
    pub fn random_nonce_hex() -> String {
        let mut raw = [0u8; 16];
        if getrandom::fill(&mut raw).is_err() {
            // Fallback: hash pid + nanos — still per-exchange unique enough to
            // bind a pong to this ping (this is a nonce, not a secret).
            let seed = format!(
                "{}:{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let digest = Sha256::digest(seed.as_bytes());
            return hex::encode(&digest[..16]);
        }
        hex::encode(raw)
    }

    fn mac_tag(key: &[u8; 32], fields: &[&str]) -> String {
        let mut msg = PROTO.to_string();
        for f in fields {
            msg.push(':');
            msg.push_str(f);
        }
        hmac_sha256_hex(key, msg.as_bytes())
    }

    /// Field must not contain the wire separator or whitespace.
    fn wire_safe(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == ',' || c == '-' || c == '_' || c == '.')
    }

    /// Stable per-node identity persisted at `~/.susi/node_id` (0600) —
    /// generated once on first use. Consensus needs distinct coordinator
    /// identities: a hardcoded node id would put every member's commit
    /// sequence, chain linkage, and election tie-breaks in one shared
    /// namespace.
    pub fn node_id() -> Option<String> {
        let path = crate::susi_paths::SusiDirs::config_dir().join("node_id");
        if let Ok(text) = fs::read_to_string(&path) {
            let id = text.trim().to_string();
            if wire_safe(&id) && id.len() <= 64 {
                return Some(id);
            }
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ok()?;
        }
        let id = format!("susi-node-{}", &random_nonce_hex()[..12]);
        // 0600 like the cluster key — the id isn't secret, but a
        // world-writable identity file invites trivial spoofing.
        #[cfg(unix)]
        let written = {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .and_then(|mut f| f.write_all(id.as_bytes()))
                .is_ok()
        };
        #[cfg(not(unix))]
        let written = fs::write(&path, &id).is_ok();
        written.then_some(id)
    }

    /// The node id this host advertises on the wire — the persisted
    /// identity, or a per-process ephemeral id when the substrate is
    /// read-only. Never a shared constant: two degraded nodes must
    /// still not collide in the coordinator/election namespace.
    pub fn wire_node_id() -> String {
        node_id().unwrap_or_else(ephemeral_node_id)
    }

    /// Process-unique fallback identity for read-only substrates —
    /// generated once per process so signed pings stay consistent
    /// while the process lives.
    fn ephemeral_node_id() -> String {
        static EPHEMERAL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        EPHEMERAL
            .get_or_init(|| format!("susi-ephemeral-{}", &random_nonce_hex()[..12]))
            .clone()
    }

    // ---- Per-member Ed25519 identity (`~/.susi/node.key`) ----
    //
    // The cluster key proves *membership* (shared symmetric secret); the
    // node key proves *which member* — non-repudiable coordinator
    // attribution. Every commit record carries `member_sig`, an Ed25519
    // signature over its HMAC `signature` field, so a stolen cluster.key
    // can no longer mint records under a bound member's identity once the
    // roster binds that member's pubkey (see `commit_log`'s attribution
    // gate). The private key never leaves the file.

    /// Path to this node's private signing key (0600, hex Ed25519 seed).
    pub fn node_key_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("node.key")
    }

    /// Load this node's Ed25519 signing key, generating + persisting a
    /// fresh one (0600) on first use. `None` on malformed file or
    /// write failure — callers degrade to unsigned-member mode rather
    /// than silently re-keying.
    fn node_signing_key() -> Option<ed25519_dalek::SigningKey> {
        let path = node_key_path();
        if let Some(seed) = key_bytes_from_file(&path) {
            return Some(ed25519_dalek::SigningKey::from_bytes(&seed));
        }
        if path.exists() {
            // Present but malformed — refuse to silently rotate identity.
            eprintln!(
                "[node.key] {} is malformed (expected 64 hex chars); refusing to auto-rotate",
                path.display()
            );
            return None;
        }
        let seed = generate_key()?;
        if !write_key_file(&path, &seed) {
            return None;
        }
        Some(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// This node's public verifying key, hex-encoded. Stable for the
    /// life of `node.key` — roster bindings and signed-pong attestation
    /// both carry this value.
    pub fn node_pubkey_hex() -> Option<String> {
        node_signing_key().map(|k| hex::encode(k.verifying_key().to_bytes()))
    }

    /// Sign `payload` with this node's key → hex Ed25519 signature.
    /// Commit records pass their HMAC `signature` as the payload, so the
    /// member signature transitively binds every field the HMAC covers.
    pub fn member_sign(payload: &str) -> Option<String> {
        let key = node_signing_key()?;
        let msg = format!("susi-member-v1:{payload}");
        Some(hex::encode(
            ed25519_dalek::Signer::sign(&key, msg.as_bytes()).to_bytes(),
        ))
    }

    /// Verify `sig_hex` under `pubkey_hex` for `payload` — strict
    /// verification (rejects non-canonical encodings and small-order
    /// keys). Any parse failure is a refusal, never a pass.
    pub fn member_verify(pubkey_hex: &str, payload: &str, sig_hex: &str) -> bool {
        let Ok(pk) = hex::decode(pubkey_hex) else {
            return false;
        };
        let Ok(sig) = hex::decode(sig_hex) else {
            return false;
        };
        let (Ok(pk_arr), Ok(sig_arr)) = (
            <[u8; 32]>::try_from(pk.as_slice()),
            <[u8; 64]>::try_from(sig.as_slice()),
        ) else {
            return false;
        };
        let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&sig_arr);
        let msg = format!("susi-member-v1:{payload}");
        vk.verify_strict(msg.as_bytes(), &signature).is_ok()
    }

    /// Verified fields of a signed ping: `(node_id, caps_csv, checksum,
    /// bloom_hex, nonce, wants_v2)` — `node_id` is empty for pre-identity
    /// senders; `wants_v2` is true for `SUSI_PING_SIG2` senders, who must
    /// be answered with a `signed_pong_v2` (pubkey-attesting) reply.
    pub type VerifiedPing = (String, String, u64, String, String, bool);

    /// Build a signed discovery ping. Returns `(wire_message, nonce)` — the nonce
    /// must be kept to verify the answering pong. `None` when no key exists or a
    /// field is not wire-safe.
    pub fn signed_ping(caps_csv: &str, checksum: u64, bloom_hex: &str) -> Option<(String, String)> {
        let key = cluster_key()?;
        let node_id = wire_node_id();
        let nonce = random_nonce_hex();
        let checksum = checksum.to_string();
        for f in [&node_id, caps_csv, &checksum, bloom_hex, &nonce] {
            if !wire_safe(f) {
                return None;
            }
        }
        let mac = mac_tag(
            &key,
            &["ping", &node_id, caps_csv, &checksum, bloom_hex, &nonce],
        );
        Some((
            format!("SUSI_PING_SIG:{node_id}:{caps_csv}:{checksum}:{bloom_hex}:{nonce}:{mac}"),
            nonce,
        ))
    }

    /// Identity-era v2 ping (`SUSI_PING_SIG2`): identical fields to v1,
    /// but asks the responder to attest its Ed25519 pubkey in the pong.
    /// Old daemons reject the unknown tag outright — callers fall back
    /// to `signed_ping` (v1) instead of dead-ending on version skew.
    pub fn signed_ping_v2(caps_csv: &str, checksum: u64, bloom_hex: &str) -> Option<(String, String)> {
        let key = cluster_key()?;
        let node_id = wire_node_id();
        let nonce = random_nonce_hex();
        let checksum = checksum.to_string();
        for f in [&node_id, caps_csv, &checksum, bloom_hex, &nonce] {
            if !wire_safe(f) {
                return None;
            }
        }
        let mac = mac_tag(
            &key,
            &["ping2", &node_id, caps_csv, &checksum, bloom_hex, &nonce],
        );
        Some((
            format!("SUSI_PING_SIG2:{node_id}:{caps_csv}:{checksum}:{bloom_hex}:{nonce}:{mac}"),
            nonce,
        ))
    }

    /// Legacy 6-field ping for dialing pre-identity daemons — a `peers
    /// add` fallback so a new node can still handshake a peer running a
    /// build that rejects the node_id field outright.
    pub fn signed_ping_legacy(
        caps_csv: &str,
        checksum: u64,
        bloom_hex: &str,
    ) -> Option<(String, String)> {
        let key = cluster_key()?;
        let nonce = random_nonce_hex();
        let checksum = checksum.to_string();
        for f in [caps_csv, &checksum, bloom_hex, &nonce] {
            if !wire_safe(f) {
                return None;
            }
        }
        let mac = mac_tag(&key, &["ping", caps_csv, &checksum, bloom_hex, &nonce]);
        Some((
            format!("SUSI_PING_SIG:{caps_csv}:{checksum}:{bloom_hex}:{nonce}:{mac}"),
            nonce,
        ))
    }

    /// Verify a signed ping; `None` for malformed input or a bad MAC —
    /// the caller must not answer. Legacy 6-field pings (pre-identity
    /// builds) verify with an empty node_id so mixed-version clusters
    /// still handshake. `SUSI_PING_SIG2` verifies identically but sets
    /// `wants_v2` — answer it with `signed_pong_v2`.
    pub fn verify_signed_ping(msg: &str) -> Option<VerifiedPing> {
        let key = cluster_key()?;
        let parts: Vec<&str> = msg.split(':').collect();
        let v2 = match parts.first() {
            Some(&"SUSI_PING_SIG2") => true,
            Some(&"SUSI_PING_SIG") => false,
            _ => return None,
        };
        // SUSI_PING_SIG[2]:[<node_id>:]<caps>:<checksum>:<bloom>:<nonce>:<mac>
        let (node_id, caps, checksum_s, bloom, nonce, mac) = match parts.len() {
            7 => (parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]),
            6 => ("", parts[1], parts[2], parts[3], parts[4], parts[5]),
            _ => return None,
        };
        for f in [caps, checksum_s, bloom, nonce] {
            if !wire_safe(f) {
                return None;
            }
        }
        if !node_id.is_empty() && !wire_safe(node_id) {
            return None;
        }
        let tag = if v2 { "ping2" } else { "ping" };
        let expected = if parts.len() == 6 {
            mac_tag(&key, &[tag, caps, checksum_s, bloom, nonce])
        } else {
            mac_tag(&key, &[tag, node_id, caps, checksum_s, bloom, nonce])
        };
        if expected != mac {
            return None;
        }
        Some((
            node_id.to_string(),
            caps.to_string(),
            checksum_s.parse().unwrap_or(0),
            bloom.to_string(),
            nonce.to_string(),
            v2,
        ))
    }

    /// Maximum roster entries carried in a signed pong — keeps the
    /// datagram well under typical UDP MTU (~1400B).
    pub const ROSTER_GOSSIP_MAX: usize = 8;

    /// Encode `(node_id, address)` gossip entries for the signed pong's
    /// roster field — `;`-joined `node_id@address`, hex-encoded so the
    /// field stays wire-safe. An empty roster encodes as `0`.
    pub fn encode_roster(entries: &[(String, String)]) -> String {
        if entries.is_empty() {
            return "0".to_string();
        }
        let joined = entries
            .iter()
            .take(ROSTER_GOSSIP_MAX)
            .map(|(id, addr)| format!("{id}@{addr}"))
            .collect::<Vec<_>>()
            .join(";");
        hex::encode(joined.as_bytes())
    }

    /// Inverse of `encode_roster`; `0` or undecodable input yields an
    /// empty list — a malformed roster never fails the handshake itself
    /// (the MAC still authenticates the field).
    pub fn decode_roster(field: &str) -> Vec<(String, String)> {
        if field == "0" {
            return Vec::new();
        }
        let Ok(bytes) = hex::decode(field) else {
            return Vec::new();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Vec::new();
        };
        text.split(';')
            .filter_map(|e| {
                e.split_once('@')
                    .map(|(id, a)| (id.to_string(), a.to_string()))
            })
            .collect()
    }

    /// Build a signed pong bound to `nonce` (echoed from the verified
    /// ping). `roster` carries `(node_id, address)` gossip so a joining
    /// node learns the rest of the cluster from one handshake —
    /// transitive membership instead of per-edge manual adds.
    pub fn signed_pong(
        node_id: &str,
        checksum: u64,
        bloom_hex: &str,
        nonce: &str,
        roster: &[(String, String)],
    ) -> Option<String> {
        let key = cluster_key()?;
        let checksum = checksum.to_string();
        // Cap the gossip roster: entries hex-encode to ~80 B each, and a
        // UDP datagram past the LAN MTU fragments or drops — a giant
        // roster would break handshakes rather than accelerate them.
        // Convergence doesn't need completeness here: anti-entropy and
        // further pongs fill in whatever this pong omits.
        let roster_hex = encode_roster(&roster[..roster.len().min(32)]);
        for f in [node_id, &checksum, bloom_hex, nonce, &roster_hex] {
            if !wire_safe(f) {
                return None;
            }
        }
        let mac = mac_tag(
            &key,
            &["pong", node_id, &checksum, bloom_hex, nonce, &roster_hex],
        );
        Some(format!(
            "SUSI_PONG_SIG:{node_id}:{checksum}:{bloom_hex}:{nonce}:{roster_hex}:{mac}"
        ))
    }

    /// V2 pong (`SUSI_PONG_SIG2`) — answers `signed_ping_v2` with this
    /// node's Ed25519 pubkey attested inside the MAC. The shared key
    /// authenticates the datagram; the pubkey inside it binds the
    /// attested `node_id` to a private key only the responder holds —
    /// the handshake half of member-signed consensus.
    pub fn signed_pong_v2(
        node_id: &str,
        checksum: u64,
        bloom_hex: &str,
        nonce: &str,
        roster: &[(String, String)],
    ) -> Option<String> {
        let key = cluster_key()?;
        let pubkey = node_pubkey_hex()?;
        let checksum = checksum.to_string();
        let roster_hex = encode_roster(&roster[..roster.len().min(32)]);
        for f in [node_id, &pubkey, &checksum, bloom_hex, nonce, &roster_hex] {
            if !wire_safe(f) {
                return None;
            }
        }
        let mac = mac_tag(
            &key,
            &[
                "pong2",
                node_id,
                &pubkey,
                &checksum,
                bloom_hex,
                nonce,
                &roster_hex,
            ],
        );
        Some(format!(
            "SUSI_PONG_SIG2:{node_id}:{pubkey}:{checksum}:{bloom_hex}:{nonce}:{roster_hex}:{mac}"
        ))
    }

    /// Verified fields of a signed pong: `(node_id, checksum, bloom_hex,
    /// roster, pubkey)` — `pubkey` is the responder's attested Ed25519
    /// verifying key, empty for pre-v2 pongs.
    pub type VerifiedPong = (String, u64, String, Vec<(String, String)>, String);

    /// Verify a signed pong against `expected_nonce`. `None` for
    /// malformed input, a bad MAC, or a nonce that does not match the
    /// nonce sent with our ping. Legacy 6-field pongs (pre-gossip
    /// builds) verify with an empty roster so mixed-version clusters
    /// still handshake; v1 7-field pongs verify with an empty pubkey.
    pub fn verify_signed_pong(msg: &str, expected_nonce: &str) -> Option<VerifiedPong> {
        let key = cluster_key()?;
        let parts: Vec<&str> = msg.split(':').collect();
        // SUSI_PONG_SIG2:<node_id>:<pubkey>:<checksum>:<bloom>:<nonce>:<roster>:<mac>
        // SUSI_PONG_SIG:<node_id>:<checksum>:<bloom>:<nonce>:[<roster>:]<mac>
        let (node_id, pubkey, checksum_s, bloom, nonce, roster_hex, mac) = match (
            parts.first(),
            parts.len(),
        ) {
            (Some(&"SUSI_PONG_SIG2"), 8) => (
                parts[1], parts[2], parts[3], parts[4], parts[5], parts[6], parts[7],
            ),
            (Some(&"SUSI_PONG_SIG"), 7) => {
                (parts[1], "", parts[2], parts[3], parts[4], parts[5], parts[6])
            }
            (Some(&"SUSI_PONG_SIG"), 6) => {
                (parts[1], "", parts[2], parts[3], parts[4], "0", parts[5])
            }
            _ => return None,
        };
        for f in [node_id, checksum_s, bloom, nonce, roster_hex] {
            if !wire_safe(f) {
                return None;
            }
        }
        // pubkey may be empty (v1 pongs) but never malformed when present.
        if !pubkey.is_empty() && !wire_safe(pubkey) {
            return None;
        }
        if nonce != expected_nonce {
            return None;
        }
        let expected = match (parts.first(), parts.len()) {
            (Some(&"SUSI_PONG_SIG2"), _) => mac_tag(
                &key,
                &["pong2", node_id, pubkey, checksum_s, bloom, nonce, roster_hex],
            ),
            (_, 6) => mac_tag(&key, &["pong", node_id, checksum_s, bloom, nonce]),
            _ => mac_tag(
                &key,
                &["pong", node_id, checksum_s, bloom, nonce, roster_hex],
            ),
        };
        if expected != mac {
            return None;
        }
        Some((
            node_id.to_string(),
            checksum_s.parse().unwrap_or(0),
            bloom.to_string(),
            decode_roster(roster_hex),
            pubkey.to_string(),
        ))
    }
}
mod json_util {
    // susi Sandbox Manager: 100% DYNAMIC - Zero hardcoded keys
    // Pattern used by Astral (ruff) and Claude Code: Registry + HashMap + Value
    // Add new model, prompt, message, endpoint without touching Rust

    use crate::susi_error::{EaiError, EaiResult};
    use serde::Serialize;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    // === CORE DYNAMIC TYPES ===
    // Everything is a registry. No struct fields are hardcoded.

    pub type DynamicValue = serde_json::Value;
    pub type DynamicRegistry = HashMap<String, DynamicValue>;
    pub type StringRegistry = HashMap<String, String>;

    // ModelTier and ProviderType are now dynamic strings, not hardcoded enums
    pub type ModelTier = String;
    pub type ProviderType = String;

    // === SHARED SELF-HEALING JSON LOAD/SAVE ===
    // Every `*.default.json`-backed config type (SusiConfig, SusiPrompts,
    // SusiMessages) needs the same two things: an atomic write (so a concurrent
    // reader never observes a torn file) and a recursive merge that backfills a
    // key/array-element present in the compiled-in default but missing from the
    // user's persisted file, without ever touching a value the user already set.
    // Factored out once here instead of three separately hand-rolled (and, until
    // this was noticed, inconsistently deep) copies.

    /// Writes `value` as pretty JSON to `path` via a same-directory temp file +
    /// rename, so a concurrent reader — another process's CLI invocation, the
    /// daemon's own background cycle — never observes a torn/empty file.
    /// On Unix the temp file is created `0600` so secrets that land in config
    /// (or adjacent host JSON) are not world-readable under a permissive umask.
    pub fn atomic_write_json_pretty<T: Serialize>(path: &Path, value: &T) -> EaiResult<()> {
        let json =
            serde_json::to_string_pretty(value).map_err(|e| EaiError::config(e.to_string()))?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        // Each writer owns a distinct file, including simultaneous writes in one process.
        let (tmp_path, mut file) = loop {
            let tmp_path = dir.join(format!(
                ".{}.tmp.{}.{}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("config"),
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
            ));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&tmp_path) {
                Ok(file) => break (tmp_path, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(EaiError::config(error.to_string())),
            }
        };
        let result = file
            .write_all(json.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&tmp_path, path));
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        #[cfg(unix)]
        if result.is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        result.map_err(|error| EaiError::config(error.to_string()))
    }

    /// Join `user_path` under `workspace`, rejecting absolutes, `..`, and escapes.
    pub fn confined_workspace_join(workspace: &Path, user_path: &str) -> EaiResult<PathBuf> {
        use std::path::Component;
        let user_path = user_path.trim().trim_matches('"').trim_matches('\'');
        let path = PathBuf::from(user_path);
        if path.is_absolute() {
            return Err(EaiError::config("Absolute paths not allowed"));
        }
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(EaiError::config("Parent directory traversal not allowed"));
            }
        }
        let full = workspace.join(&path);
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|e| EaiError::config(format!("Workspace error: {e}")))?;
        // For not-yet-existing leaves, canonicalize the parent and re-join the name.
        let parent = full.parent().unwrap_or(workspace);
        let canonical_parent = parent
            .canonicalize()
            .unwrap_or_else(|_| canonical_workspace.clone());
        if !canonical_parent.starts_with(&canonical_workspace) {
            return Err(EaiError::config(format!(
                "Path escape attempt: {user_path}"
            )));
        }
        let leaf = full
            .file_name()
            .ok_or_else(|| EaiError::config(format!("Path has no file name: {user_path}")))?;
        Ok(canonical_parent.join(leaf))
    }

    /// Recursively backfills any key (object) or element (same-length array)
    /// present in `default` but absent from `existing`. Returns whether
    /// `existing` was modified. A value `existing` already has is never
    /// overwritten, at any nesting depth.
    pub fn merge_missing_json_defaults(
        existing: &mut DynamicValue,
        default: &DynamicValue,
    ) -> bool {
        match (existing, default) {
            (DynamicValue::Object(existing_map), DynamicValue::Object(default_map)) => {
                let mut changed = false;
                for (k, def_v) in default_map {
                    match existing_map.get_mut(k) {
                        Some(existing_v) => {
                            if merge_missing_json_defaults(existing_v, def_v) {
                                changed = true;
                            }
                        }
                        None => {
                            existing_map.insert(k.clone(), def_v.clone());
                            changed = true;
                        }
                    }
                }
                changed
            }
            (DynamicValue::Array(existing_arr), DynamicValue::Array(default_arr))
                if existing_arr.len() == default_arr.len() =>
            {
                let mut changed = false;
                for (e, d) in existing_arr.iter_mut().zip(default_arr.iter()) {
                    if merge_missing_json_defaults(e, d) {
                        changed = true;
                    }
                }
                changed
            }
            _ => false,
        }
    }

    /// Backfills into `existing` any top-level key present in `default` but
    /// missing, and recursively self-heals nested values (via
    /// `merge_missing_json_defaults`) for keys both sides already have. Returns
    /// whether `existing` changed.
    pub fn merge_missing_registry_defaults(
        existing: &mut DynamicRegistry,
        default: &DynamicRegistry,
    ) -> bool {
        let mut changed = false;
        for (key, default_val) in default {
            match existing.get_mut(key) {
                Some(existing_val) => {
                    if merge_missing_json_defaults(existing_val, default_val) {
                        changed = true;
                    }
                }
                None => {
                    existing.insert(key.clone(), default_val.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    /// Shared ureq Agent with connect/read/write timeouts. `ureq::get`/`ureq::post`
    /// free functions use a default agent with NO timeouts at all — a stalled
    /// remote (or one that completes the handshake but then goes silent
    /// mid-response, e.g. during SSE body streaming) blocks the calling thread
    /// forever. Agent-level timeout_read/timeout_write bound every socket read
    /// and write, including streaming body reads after the initial response
    /// headers arrive, which a per-request `.timeout()` alone would not cover.
    static HTTP_AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();

    pub fn http_agent() -> ureq::Agent {
        HTTP_AGENT
            .get_or_init(|| {
                let config = ureq::Agent::config_builder()
                    .timeout_connect(Some(std::time::Duration::from_secs(10)))
                    .timeout_recv_body(Some(std::time::Duration::from_secs(20)))
                    .timeout_send_body(Some(std::time::Duration::from_secs(20)))
                    .build();
                ureq::Agent::new_with_config(config)
            })
            .clone()
    }
}
mod types {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    use crate::susi_config::json_util::{
        merge_missing_json_defaults, merge_missing_registry_defaults, DynamicRegistry,
        DynamicValue, StringRegistry,
    };

    pub type RiskTier = String; // Was enum, now dynamic: "Tier0ZeroRisk", "Tier1LowRisk", etc.

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct DynamicModelInfo {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
    }

    impl DynamicModelInfo {
        // Each parameter maps 1:1 to a distinct serialized field (name/registry/
        // model_id/...); a builder would just move the same arity into chained
        // calls at every construction site for no behavioral benefit.
        #[allow(clippy::too_many_arguments)]
        pub fn new(
            name: String,
            registry: String,
            model_id: String,
            description: String,
            is_local: bool,
            tier: String,
            latency_ms: Option<u128>,
            provider: String,
            checksum: Option<String>,
            provenance: Option<serde_json::Value>,
        ) -> Self {
            let mut fields = DynamicRegistry::new();
            fields.insert("name".to_string(), serde_json::json!(name));
            fields.insert("registry".to_string(), serde_json::json!(registry));
            fields.insert("model_id".to_string(), serde_json::json!(model_id));
            fields.insert("description".to_string(), serde_json::json!(description));
            fields.insert("is_local".to_string(), serde_json::json!(is_local));
            fields.insert("tier".to_string(), serde_json::json!(tier));
            if let Some(lat) = latency_ms {
                fields.insert("latency_ms".to_string(), serde_json::json!(lat));
            }
            fields.insert("provider".to_string(), serde_json::json!(provider));
            if let Some(chk) = checksum {
                fields.insert("checksum".to_string(), serde_json::json!(chk));
            }
            if let Some(prov) = provenance {
                fields.insert("provenance".to_string(), prov);
            }
            Self { fields }
        }

        pub fn get(&self, key: &str) -> Option<&DynamicValue> {
            self.fields.get(key)
        }
        pub fn get_str(&self, key: &str) -> Option<&str> {
            self.fields.get(key)?.as_str()
        }
        pub fn get_bool(&self, key: &str) -> Option<bool> {
            self.fields.get(key)?.as_bool()
        }
        pub fn name(&self) -> &str {
            self.get_str("name")
                .or_else(|| self.get_str("model_id"))
                .unwrap_or("unknown")
        }
        pub fn model_id(&self) -> &str {
            self.get_str("model_id")
                .or_else(|| self.get_str("id"))
                .or_else(|| self.get_str("name"))
                .unwrap_or("unknown")
        }
        pub fn provider(&self) -> &str {
            self.get_str("provider").unwrap_or("unknown")
        }
        pub fn is_local(&self) -> bool {
            self.get_bool("is_local").unwrap_or(false)
        }
        pub fn registry(&self) -> &str {
            self.get_str("registry").unwrap_or("SUSI Substrate")
        }
        pub fn description(&self) -> &str {
            self.get_str("description").unwrap_or("")
        }
        pub fn tier(&self) -> &str {
            self.get_str("tier").unwrap_or("Reflex")
        }
        pub fn latency_ms(&self) -> Option<u128> {
            self.fields
                .get("latency_ms")
                .and_then(|v| v.as_u64())
                .map(|u| u as u128)
        }
        pub fn checksum(&self) -> Option<&str> {
            self.get_str("checksum")
        }
        pub fn provenance(&self) -> Option<&DynamicValue> {
            self.get("provenance")
        }
    }

    // Backward compat alias
    pub type ModelInfo = DynamicModelInfo;

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct DynamicStagedFix {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
    }

    impl DynamicStagedFix {
        pub fn file_path(&self) -> &str {
            self.fields
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn original_content(&self) -> &str {
            self.fields
                .get("original_content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn staged_content(&self) -> &str {
            self.fields
                .get("staged_content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn description(&self) -> &str {
            self.fields
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
    }

    pub type StagedFix = DynamicStagedFix;

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct DynamicIntentBundle {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        #[serde(default)]
        pub staged_fixes: Vec<DynamicStagedFix>,
        #[serde(default)]
        pub applied: bool,
        #[serde(default)]
        pub title: String,
    }

    impl DynamicIntentBundle {
        pub fn bundle_id(&self) -> &str {
            self.fields
                .get("bundle_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn title(&self) -> &str {
            if !self.title.is_empty() {
                &self.title
            } else {
                self.fields
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            }
        }
        pub fn risk_tier(&self) -> RiskTier {
            self.fields
                .get("risk_tier")
                .and_then(|v| v.as_str())
                .unwrap_or("Tier0ZeroRisk")
                .to_string()
        }
        pub fn description(&self) -> &str {
            self.fields
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn is_applied(&self) -> bool {
            self.applied
                || self
                    .fields
                    .get("applied")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        }
        pub fn set_applied(&mut self, val: bool) {
            self.applied = val;
            self.fields.remove("applied");
        }
    }

    pub type IntentBundle = DynamicIntentBundle;

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct DynamicNeuralCheckpoint {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
    }

    impl DynamicNeuralCheckpoint {
        pub fn intent(&self) -> &str {
            self.fields
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
        pub fn timestamp(&self) -> u64 {
            self.fields
                .get("timestamp")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        }
        pub fn completed_tools(&self) -> Vec<String> {
            self.fields
                .get("completed_tools")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        }
        pub fn status(&self) -> &str {
            self.fields
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("IN_PROGRESS")
        }
    }

    pub type NeuralCheckpoint = DynamicNeuralCheckpoint;

    // === 100% DYNAMIC CHAT TEMPLATES ===
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(default)]
    pub struct ChatTemplateConfig {
        #[serde(flatten)]
        pub templates: StringRegistry,
    }

    impl Default for ChatTemplateConfig {
        #[allow(clippy::expect_used)]
        fn default() -> Self {
            // Deserialize directly into the flattened map type, not `Self` — the
            // container's #[serde(default)] makes Self's Deserialize impl call
            // Self::default() to backfill missing fields, which would recurse
            // infinitely (stack overflow) if this constructed a Self via serde_json.
            // Mandate 42: safe - this and the other bundled-default `.expect()`
            // calls in this file (default_dynamic x2, SusiConfig::default) all
            // deserialize a file compiled into the binary via include_str!, not
            // a user-editable runtime file. Content is fixed for a given
            // binary, so parsing either always succeeds or always fails for
            // that binary - a failure is a build/packaging bug caught by any
            // test run, never a runtime condition that varies between calls.
            let templates: StringRegistry = serde_json::from_str(include_str!(
            "../../../config/chat_templates.default.json"
        ))
        .expect(
            "Fatal: chat_templates.default.json must be valid JSON. Zero hardcoded config allowed.",
        );
            Self { templates }
        }
    }

    impl ChatTemplateConfig {
        pub fn from_file(path: &str) -> Self {
            let p = Path::new(path);
            if p.is_file() {
                if let Ok(content) = fs::read_to_string(p) {
                    if let Ok(cfg) = serde_json::from_str::<ChatTemplateConfig>(&content) {
                        return cfg;
                    }
                    if let Ok(map) = serde_json::from_str::<StringRegistry>(&content) {
                        return Self { templates: map };
                    }
                }
            }
            Self::default()
        }

        pub fn get(&self, model_name: &str) -> Option<&String> {
            if let Some(t) = self.templates.get(model_name) {
                return Some(t);
            }
            let lower = model_name.to_lowercase().replace(['-', '_', '.'], "");
            for (k, v) in &self.templates {
                let k_clean = k.to_lowercase().replace(['-', '_', '.'], "");
                if lower.contains(&k_clean) || k_clean.contains(&lower) {
                    return Some(v);
                }
            }
            self.templates
                .get("chatml")
                .or_else(|| self.templates.values().next())
        }

        // 100% dynamic render - supports ANY {variable}
        pub fn render(&self, model_name: &str, vars: &HashMap<String, String>) -> String {
            let template = self
                .get(model_name)
                .cloned()
                .unwrap_or_else(|| "{system}\n{prompt}".to_string());
            let mut out = template;
            for (k, v) in vars {
                out = out.replace(&format!("{{{}}}", k), v);
            }
            out
        }

        pub fn register(&mut self, name: String, template: String) {
            self.templates.insert(name, template);
        }
    }

    // === 100% DYNAMIC PROMPTS - NO HARDCODED FIELDS ===
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct SusiPrompts {
        #[serde(flatten)]
        pub prompts: DynamicRegistry,
        #[serde(default)]
        pub chat_templates: ChatTemplateConfig,
    }

    impl SusiPrompts {
        pub fn load_global() -> Self {
            let prompts_file = crate::susi_paths::SusiDirs::config_dir().join("prompts.json");

            static STORE: std::sync::OnceLock<
                crate::susi_config::versioned_store::VersionedJsonStore<SusiPrompts>,
            > = std::sync::OnceLock::new();
            let store =
                STORE.get_or_init(crate::susi_config::versioned_store::VersionedJsonStore::new);

            let mut prompts = store
                .load_with_healing(
                    &prompts_file,
                    || Ok(Self::default_dynamic()),
                    |cfg| {
                        let default_prompts = Self::default_dynamic();
                        merge_missing_registry_defaults(&mut cfg.prompts, &default_prompts.prompts)
                    },
                    false,
                )
                .unwrap_or_else(|_| Self::default_dynamic());

            // User-editable chat-template override, hot-reloaded on every load (Mandate 15:
            // Registry Hot-Reload) independent of prompts.json's persisted snapshot.
            let templates_override =
                crate::susi_paths::SusiDirs::config_dir().join("chat_templates.json");
            if templates_override.is_file() {
                prompts.chat_templates =
                    ChatTemplateConfig::from_file(templates_override.to_str().unwrap_or_default());
            }
            prompts
        }

        #[allow(clippy::expect_used)]
        fn default_dynamic() -> Self {
            // Mandate 42: safe - see ChatTemplateConfig::default's comment
            // above; same compile-time include_str! pattern.
            let prompts: DynamicRegistry = serde_json::from_str(include_str!(
                "../../../config/prompts.default.json"
            ))
            .expect(
                "Fatal: prompts.default.json must be valid JSON. Zero hardcoded config allowed.",
            );
            Self {
                prompts,
                chat_templates: ChatTemplateConfig::default(),
            }
        }

        pub fn get(&self, key: &str) -> Option<&DynamicValue> {
            self.prompts.get(key)
        }
        pub fn get_str(&self, key: &str) -> Option<&str> {
            self.prompts.get(key)?.as_str()
        }

        pub fn agent_factory_prompt(&self) -> String {
            self.get_str("agent_factory_prompt")
                .unwrap_or("")
                .to_string()
        }
        pub fn consensus_wisdom_prompt(&self) -> String {
            self.get_str("consensus_wisdom_prompt")
                .unwrap_or("")
                .to_string()
        }
        pub fn intent_planner_prompt(&self) -> String {
            self.get_str("intent_planner_prompt")
                .unwrap_or("")
                .to_string()
        }
        pub fn mission_partition_prompt(&self) -> String {
            self.get_str("mission_partition_prompt")
                .unwrap_or("")
                .to_string()
        }
        /// Used by `DynamicAgent` (every synthesized specialist, plus the
        /// UniversalReasoner fallback). Deliberately asks for a direct answer,
        /// not a tool-call schema: nothing in the swarm ever parses an
        /// "action"/"action_input" field from an agent's raw output to execute a
        /// real tool and fill in "observation," so asking a small model to
        /// produce that schema just adds an unnecessary indirection it often
        /// can't complete - verified live, this produced JSON stubs describing
        /// an intended action with an empty "observation" instead of an answer.
        pub fn dynamic_agent_prompt(&self) -> String {
            self.get_str("dynamic_agent_prompt")
                .unwrap_or("")
                .to_string()
        }
        pub fn mission_refine_prompt(&self) -> String {
            self.get_str("mission_refine_prompt")
                .unwrap_or("")
                .to_string()
        }

        pub fn format_chat_prompt(
            &self,
            model_name: &str,
            system_prompt: &str,
            user_prompt: &str,
        ) -> String {
            let vars = HashMap::from([
                ("system".to_string(), system_prompt.to_string()),
                ("prompt".to_string(), user_prompt.to_string()),
            ]);
            self.chat_templates.render(model_name, &vars)
        }

        pub fn format_dynamic(&self, model_name: &str, vars: HashMap<String, String>) -> String {
            self.chat_templates.render(model_name, &vars)
        }
    }

    // === 100% DYNAMIC MESSAGES - NO HARDCODED STRUCTS ===
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct SusiMessages {
        #[serde(flatten)]
        pub categories: HashMap<String, StringRegistry>, // category -> key -> message, fully dynamic
    }

    impl SusiMessages {
        pub fn load_global() -> Self {
            let msgs_file = crate::susi_paths::SusiDirs::config_dir().join("messages.json");

            static STORE: std::sync::OnceLock<
                crate::susi_config::versioned_store::VersionedJsonStore<SusiMessages>,
            > = std::sync::OnceLock::new();
            let store =
                STORE.get_or_init(crate::susi_config::versioned_store::VersionedJsonStore::new);

            store
                .load_with_healing(
                    &msgs_file,
                    || Ok(Self::default_dynamic()),
                    |m| {
                        let default_msgs = Self::default_dynamic();
                        let mut existing_val =
                            serde_json::to_value(&*m).unwrap_or(DynamicValue::Null);
                        let default_val =
                            serde_json::to_value(&default_msgs).unwrap_or(DynamicValue::Null);

                        if merge_missing_json_defaults(&mut existing_val, &default_val) {
                            if let Ok(merged) = serde_json::from_value::<SusiMessages>(existing_val)
                            {
                                *m = merged;
                                return true;
                            }
                        }
                        false
                    },
                    false,
                )
                .unwrap_or_else(|_| Self::default_dynamic())
        }

        #[allow(clippy::expect_used)]
        fn default_dynamic() -> Self {
            // Mandate 42: safe - see ChatTemplateConfig::default's comment
            // above; same compile-time include_str! pattern.
            let categories: HashMap<String, StringRegistry> = serde_json::from_str(include_str!(
                "../../../config/messages.default.json"
            ))
            .expect(
                "Fatal: messages.default.json must be valid JSON. Zero hardcoded config allowed.",
            );
            Self { categories }
        }

        pub fn get(&self, category: &str, key: &str) -> Option<&String> {
            self.categories.get(category)?.get(key)
        }
        pub fn register(&mut self, category: String, key: String, message: String) {
            self.categories
                .entry(category)
                .or_default()
                .insert(key, message);
        }
    }

    // === 100% DYNAMIC REMAINING CONFIGS ===
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct AdminPulsesConfig {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        #[serde(default)]
        pub install_pulse: String,
        #[serde(default)]
        pub uninstall_pulse: String,
        #[serde(default)]
        pub select_model_pulse: String,
        #[serde(default)]
        pub deep_scan_pulse: String,
        #[serde(default)]
        pub mcp_scout_pulse: String,
        #[serde(default)]
        pub audit_pulse: String,
        #[serde(default)]
        pub verify_pulse: String,
        #[serde(default)]
        pub lint_pulse: String,
        #[serde(default)]
        pub audit_deps_pulse: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct InferenceEndpointItem {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        #[serde(default)]
        pub name: String,
        #[serde(default)]
        pub api_base: String,
        #[serde(default)]
        pub protocol_type: String,
        /// Default model id for this endpoint (e.g. `gpt-4o-mini`, `claude-3-5-haiku-…`).
        #[serde(default)]
        pub model: String,
        /// Env var holding the API key (e.g. `OPENAI_API_KEY`). Empty = no auth.
        #[serde(default)]
        pub api_key_env: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct InferenceEndpointsConfig {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        #[serde(default)]
        pub endpoints: Vec<InferenceEndpointItem>,
    }

    impl InferenceEndpointsConfig {}

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct ModelScoringHeuristics {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        pub size_gb_multipliers: HashMap<String, f32>,
        pub native_candle_bonus: f32,
        pub system_ram_buffer_gb: f32,
    }

    /// Tunable thresholds for when `SusiMemory::save_interaction` promotes an
    /// interaction from plain memory into the reasoning-experience distillation
    /// log (previously a hardcoded `output.len() > 50` + two magic substrings).
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct MemoryExperienceHeuristics {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        pub min_output_len: usize,
        pub failure_markers: Vec<String>,
    }

    /// Config-driven natural-intent classification for `SusiAdmin::classify_natural_intent`
    /// (Mandate 35: no hardcoded keyword lists in Rust).
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct IntentClassifyConfig {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        pub query_exact: Vec<String>,
        pub query_prefixes: Vec<String>,
        pub query_contains: Vec<String>,
        pub motion_contains: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct GovernancePatterns {
        #[serde(flatten)]
        pub patterns: DynamicRegistry,
        #[serde(default)]
        pub secret_tokens: Vec<String>,
        #[serde(default)]
        pub destructive_commands: Vec<String>,
        #[serde(default)]
        pub critical_system_paths: Vec<String>,
        #[serde(default)]
        pub exfiltration_vectors: Vec<String>,
    }

    impl GovernancePatterns {
        pub fn secret_tokens(&self) -> Vec<String> {
            self.secret_tokens.clone()
        }
        pub fn destructive_commands(&self) -> Vec<String> {
            self.destructive_commands.clone()
        }
        pub fn critical_system_paths(&self) -> Vec<String> {
            self.critical_system_paths.clone()
        }
        pub fn exfiltration_vectors(&self) -> Vec<String> {
            self.exfiltration_vectors.clone()
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct ModelLadderConfigStep {
        #[serde(flatten)]
        pub fields: DynamicRegistry,
        #[serde(default)]
        pub step: usize,
        #[serde(default)]
        pub label: String,
        #[serde(default)]
        pub hf_repo: String,
        #[serde(default)]
        pub hf_file: String,
        #[serde(default)]
        pub tokenizer_repo: String,
        #[serde(default)]
        pub min_ram_gb: f32,
        #[serde(default)]
        pub min_bytes: u64,
        #[serde(default)]
        pub expected_bytes: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    pub struct ModelLifecycleConfig {
        pub download_attempts: u32,
        pub download_timeout_secs: u64,
        pub max_parallel_downloads: usize,
        pub discovery_refresh_secs: u64,
        pub discovery_retry_secs: u64,
        pub discovery_limit: usize,
        pub max_ladder_tiers: usize,
        pub ladder_size_ratio: f32,
        pub memory_overhead_ratio: f32,
        pub prefetch_tiers: usize,
        pub failure_cooldown_secs: u64,
    }

    /// When local inference is too slow, escalate to cloud providers.
    /// `policy`: `auto` | `local_only` | `cloud_first` | `ask`
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(default)]
    pub struct InferenceRoutingConfig {
        pub policy: String,
        pub local_min_tokens_per_sec: f32,
        pub local_max_latency_ms: u64,
        pub prefer_cloud_when_cpu_only: bool,
        pub ask_when_multiple_clouds: bool,
    }

    /// Edge privacy controls: local-only inference and egress consent.
    /// `mode`: `balanced` | `local_only` | `open`
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(default)]
    pub struct PrivacyConfig {
        pub mode: String,
        /// When true (or mode is local_only), host `exec_command` requires
        /// `process.exec` grant; prefer Docker `sandbox_exec`.
        pub mandatory_sandbox_for_exec: bool,
        /// When true (default), `network.egress` needs an explicit capability grant
        /// or `susi privacy consent --egress`.
        pub require_egress_consent: bool,
    }

    impl Default for PrivacyConfig {
        fn default() -> Self {
            Self {
                mode: "balanced".into(),
                mandatory_sandbox_for_exec: false,
                require_egress_consent: true,
            }
        }
    }

    /// Config-driven external coding-agent peer (Claude Code, Cursor, Codex, …).
    ///
    /// Open admission: any name + protocol in `external_peer_agents` mounts as a
    /// GawdAgent. Protocols: `managed`, `cli` (default), `openai_chat`, `http`, `a2a`.
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct ExternalPeerAgentSpec {
        pub name: String,
        pub description: String,
        /// Wire protocol: `managed` | `cli` | `openai_chat` | `http` | `a2a` (default `cli`).
        pub protocol: String,
        /// Base URL for `openai_chat` / `http` / `a2a` peers (OpenAI-compat or A2A).
        pub api_base: String,
        /// Optional model id for `openai_chat` peers.
        pub model: String,
        /// Binaries probed in order (`which`-style) to select the live driver.
        pub detect_bins: Vec<String>,
        /// Explicit command; when empty, the first detected bin is used.
        pub command: String,
        /// Argv templates; `{goal}` and `{workspace}` are substituted.
        pub args: Vec<String>,
        pub timeout_secs: u64,
        /// When set, the named env var must be present (e.g. Devin API key).
        pub api_key_env: Option<String>,
    }

    /// One entry in the leading-models catalog (`config/models.catalog.default.json`).
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct ModelCatalogEntry {
        pub id: String,
        /// Inference endpoint name from `inference_endpoints` (e.g. OpenRouter).
        pub engine: String,
        #[serde(default)]
        pub protocol_type: String,
        #[serde(default)]
        pub api_key_env: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    #[serde(default)]
    pub struct ModelCatalogConfig {
        pub models: Vec<ModelCatalogEntry>,
    }

    impl Default for InferenceRoutingConfig {
        fn default() -> Self {
            Self {
                policy: "auto".to_string(),
                local_min_tokens_per_sec: 8.0,
                local_max_latency_ms: 15_000,
                prefer_cloud_when_cpu_only: true,
                ask_when_multiple_clouds: true,
            }
        }
    }

    // === 100% DYNAMIC SUSI CONFIG - THE ROOT ===
}
pub mod versioned_store {
    use parking_lot::RwLock;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::SystemTime;

    type CacheEntry<T> = (SystemTime, PathBuf, Arc<T>);

    /// Generic "versioned JSON config store" abstraction that unifies mtime-cache
    /// and self-healing-merge logic originally hand-rolled in five places.
    ///
    /// The cache holds `Arc<T>` so hot paths can share a snapshot without cloning
    /// the full registry on every `load_*` hit.
    pub struct VersionedJsonStore<T> {
        cache: RwLock<Option<CacheEntry<T>>>,
    }

    impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> VersionedJsonStore<T> {
        pub const fn new() -> Self {
            Self {
                cache: RwLock::new(None),
            }
        }

        pub fn load_with_healing<F, H>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
        ) -> crate::susi_error::EaiResult<T>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
        {
            Ok((*self.load_arc_with_healing(path, on_missing, heal_fn, strict_parse)?).clone())
        }

        /// Same as [`Self::load_with_healing`] but returns a shared `Arc` so cache
        /// hits avoid cloning the full value tree.
        pub fn load_arc_with_healing<F, H>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
        ) -> crate::susi_error::EaiResult<Arc<T>>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
        {
            let current_modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            {
                let guard = self.cache.read();
                if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
                    if cached_path == path
                        && *cached_time == current_modified
                        && current_modified != SystemTime::UNIX_EPOCH
                    {
                        return Ok(Arc::clone(cached_val));
                    }
                }
            }

            let mut guard = self.cache.write();

            let current_modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
                if cached_path == path
                    && *cached_time == current_modified
                    && current_modified != SystemTime::UNIX_EPOCH
                {
                    return Ok(Arc::clone(cached_val));
                }
            }

            let val = Arc::new(self.read_from_disk(path, on_missing, heal_fn, strict_parse)?);

            let final_modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(current_modified);

            *guard = Some((final_modified, path.to_path_buf(), Arc::clone(&val)));
            Ok(val)
        }

        /// Reads from disk even if its modification time matches the cached version.
        /// Updates the cache under the same lock used by normal reads and mutations.
        pub fn reload_with_healing<F, H>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
        ) -> crate::susi_error::EaiResult<T>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
        {
            Ok((*self.reload_arc_with_healing(path, on_missing, heal_fn, strict_parse)?).clone())
        }

        pub fn reload_arc_with_healing<F, H>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
        ) -> crate::susi_error::EaiResult<Arc<T>>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
        {
            let mut guard = self.cache.write();
            // A failed reload must not leave a previously valid value cached.
            *guard = None;
            let value = Arc::new(self.read_from_disk(path, on_missing, heal_fn, strict_parse)?);
            let modified = std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            *guard = Some((modified, path.to_path_buf(), Arc::clone(&value)));
            Ok(value)
        }

        /// Modifies the JSON configuration in-place, synchronizing the change to disk.
        /// Uses an exclusive lock to prevent process-local race conditions.
        #[allow(clippy::too_many_arguments)]
        pub fn modify<F, H, M>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
            mut_fn: M,
        ) -> crate::susi_error::EaiResult<T>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
            M: FnOnce(&mut T),
        {
            let mut guard = self.cache.write();

            let current_modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            let mut val = if let Some((cached_time, cached_path, cached_val)) = guard.as_ref() {
                if cached_path == path
                    && *cached_time == current_modified
                    && current_modified != SystemTime::UNIX_EPOCH
                {
                    (**cached_val).clone()
                } else {
                    self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
                }
            } else {
                self.read_from_disk(path, on_missing, heal_fn, strict_parse)?
            };

            mut_fn(&mut val);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::susi_config::atomic_write_json_pretty(path, &val)?;

            let final_modified = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(current_modified);

            let shared = Arc::new(val.clone());
            *guard = Some((final_modified, path.to_path_buf(), Arc::clone(&shared)));
            Ok(val)
        }

        fn read_from_disk<F, H>(
            &self,
            path: &Path,
            on_missing: F,
            heal_fn: H,
            strict_parse: bool,
        ) -> crate::susi_error::EaiResult<T>
        where
            F: FnOnce() -> crate::susi_error::EaiResult<T>,
            H: FnOnce(&mut T) -> bool,
        {
            let mut loaded_opt = None;
            if path.is_file() {
                match std::fs::read_to_string(path) {
                    Ok(content) => match serde_json::from_str::<T>(&content) {
                        Ok(val) => loaded_opt = Some(val),
                        Err(e) => {
                            if strict_parse {
                                return Err(crate::susi_error::EaiError::config(format!(
                                    "Malformed configuration {}: {}",
                                    path.display(),
                                    e
                                )));
                            }
                        }
                    },
                    Err(e) => {
                        if strict_parse {
                            return Err(crate::susi_error::EaiError::config(format!(
                                "Failed to read {}: {}",
                                path.display(),
                                e
                            )));
                        }
                    }
                }
            }

            let (mut val, mut needs_save) = match loaded_opt {
                Some(v) => (v, false),
                None => {
                    let default_val = on_missing()?;
                    (default_val, true)
                }
            };

            if heal_fn(&mut val) {
                needs_save = true;
            }

            if needs_save {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::susi_config::atomic_write_json_pretty(path, &val)?;
            }

            Ok(val)
        }
    }

    impl<T: Clone + serde::de::DeserializeOwned + serde::Serialize> Default for VersionedJsonStore<T> {
        fn default() -> Self {
            Self::new()
        }
    }
}
pub mod extensions {
    use parking_lot::Mutex;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Active extension pack (id + host root).
    #[derive(Debug, Clone)]
    pub struct ExtensionPack {
        pub id: String,
        /// Host directory (`~/.susi/extensions/<id>`).
        pub root: PathBuf,
    }

    /// Pack status for listing / CLI.
    #[derive(Debug, Clone, Serialize)]
    pub struct PackStatus {
        pub id: String,
        pub name: String,
        pub version: String,
        pub seeded: bool,
        pub loaded: bool,
        pub active: bool,
        pub root: PathBuf,
    }

    /// Pack manifest (`manifest.json`).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ExtensionManifest {
        pub id: String,
        pub name: String,
        pub version: String,
        /// Host API major compatibility (`1` ↔ host major `1`). Default `1`.
        #[serde(default = "default_api_version", alias = "apiVersion")]
        pub api_version: String,
        #[serde(default)]
        pub description: String,
        /// Declared capability ids (e.g. `catalog.coding_models`).
        #[serde(default)]
        pub capabilities: Vec<String>,
        /// Required host capabilities (hard fail if missing).
        #[serde(default)]
        pub requires: Vec<String>,
        /// Optional host capabilities (warn / degrade if missing).
        #[serde(default)]
        pub optional: Vec<String>,
        /// Declared permissions — enforced: unknown strings and `files` entries
        /// escaping the pack root without `filesystem.read` are rejected at load.
        #[serde(default)]
        pub permissions: Vec<String>,
        /// Logical file name → path relative to the pack root.
        #[serde(default)]
        pub files: BTreeMap<String, String>,
        /// Mandate 35: preserve unknown pack-manifest keys.
        #[serde(flatten, default)]
        pub extra: HashMap<String, serde_json::Value>,
    }

    fn default_api_version() -> String {
        "1".into()
    }

    /// Host API major this binary supports for extension packs.
    pub const HOST_PACK_API_MAJOR: u32 = 1;

    /// Permission vocabulary the host can enforce. Unknown strings are rejected —
    /// a permission the host cannot enforce must never be silently granted.
    pub const KNOWN_PERMISSIONS: &[&str] = &[
        "filesystem.read",
        "filesystem.write",
        "network.egress",
        "process.exec",
    ];

    /// True when `rel` resolves outside the pack root: absolute paths always
    /// escape; a `..` that climbs above the root escapes (e.g. the bundled default
    /// pack's `../../coding-models.json`, which is permitted only because it
    /// declares `filesystem.read`).
    fn path_escapes_pack_root(rel: &str) -> bool {
        let path = Path::new(rel);
        if path.is_absolute() {
            return true;
        }
        let mut depth: u32 = 0;
        for comp in path.components() {
            match comp {
                std::path::Component::ParentDir => {
                    if depth == 0 {
                        return true;
                    }
                    depth -= 1;
                }
                std::path::Component::Normal(_) => depth += 1,
                std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::CurDir => {}
            }
        }
        false
    }

    /// Validate a pack manifest for load. Returns `Ok(())` or a human error.
    /// Incompatible `api_version` majors fail; missing optional deps do not.
    /// Permissions are enforced: unknown permission strings and `files` entries
    /// that escape the pack root without declaring `filesystem.read` are rejected.
    pub fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), String> {
        if manifest.id.trim().is_empty() {
            return Err("pack manifest id must not be empty".into());
        }
        let major = manifest
            .api_version
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if major != HOST_PACK_API_MAJOR {
            return Err(format!(
                "pack `{}` api_version {} incompatible with host major {}",
                manifest.id, manifest.api_version, HOST_PACK_API_MAJOR
            ));
        }
        for perm in &manifest.permissions {
            if perm.trim().is_empty() {
                return Err(format!(
                    "pack `{}` has an empty permissions entry",
                    manifest.id
                ));
            }
            if !KNOWN_PERMISSIONS.contains(&perm.as_str()) {
                return Err(format!(
                    "pack `{}` declares unknown permission `{perm}` (known: {})",
                    manifest.id,
                    KNOWN_PERMISSIONS.join(", ")
                ));
            }
        }
        let can_read_outside = manifest.permissions.iter().any(|p| p == "filesystem.read");
        for (logical, rel) in &manifest.files {
            if Path::new(rel).is_absolute() {
                return Err(format!(
                "pack `{id}` maps `{logical}` to an absolute path `{rel}` — pack files must be relative to the pack root",
                id = manifest.id
            ));
            }
            if path_escapes_pack_root(rel) && !can_read_outside {
                return Err(format!(
                "pack `{id}` maps `{logical}` outside its root (`{rel}`) without declaring `filesystem.read`",
                id = manifest.id
            ));
            }
        }
        Ok(())
    }

    /// Durable substrate state (`~/.susi/extensions/state.json`).
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ExtensionsState {
        #[serde(default = "default_pack_id")]
        active: String,
        #[serde(default)]
        loaded: Vec<String>,
        /// Packs the user unloaded — not auto-loaded on discover until `load_pack`.
        #[serde(default)]
        unloaded: Vec<String>,
        /// Mandate 35: preserve unknown state keys on write-back.
        #[serde(flatten, default)]
        extra: HashMap<String, serde_json::Value>,
    }

    fn default_pack_id() -> String {
        DEFAULT_PACK_ID.to_string()
    }

    impl Default for ExtensionsState {
        fn default() -> Self {
            Self {
                active: DEFAULT_PACK_ID.to_string(),
                loaded: vec![DEFAULT_PACK_ID.to_string()],
                unloaded: Vec::new(),
                extra: HashMap::new(),
            }
        }
    }

    /// One cloud vendor entry from pack `cloud-vendors.json`.
    #[derive(Debug, Clone, Deserialize)]
    pub struct CloudVendorEntry {
        pub id: String,
        #[serde(default)]
        pub aliases: Vec<String>,
        pub api_key_env: String,
        #[serde(default)]
        pub api_key_env_alts: Vec<String>,
    }

    const DEFAULT_PACK_ID: &str = "default";
    const BUNDLED_MANIFEST: &str = include_str!("../../../config/extensions/default/manifest.json");
    const BUNDLED_CLOUD_VENDORS: &str =
        include_str!("../../../config/extensions/default/cloud-vendors.json");
    const BUNDLED_CODING_MODELS: &str = include_str!("../../../config/coding-models.json");
    const BUNDLED_EXECUTION_AGENTS: &str = include_str!("../../../config/execution-agents.json");
    const BUNDLED_AGENT_ENGINES: &str = include_str!("../../../config/agent-engines.json");
    const BUNDLED_LEADING_MCP: &str = include_str!("../../../config/leading-mcp.json");
    const BUNDLED_MODELS_CATALOG: &str =
        include_str!("../../../config/models.catalog.default.json");
    const BUNDLED_CONFIG_DEFAULT: &str = include_str!("../../../config/config.default.json");
    const BUNDLED_OPENROUTER_MODELS: &str = include_str!("../../../config/openrouter-models.json");
    const BUNDLED_OPEN_WEIGHT_MODELS: &str =
        include_str!("../../../config/open-weight-models.json");
    const BUNDLED_FRONTIER_MODELS: &str = include_str!("../../../config/frontier-models.json");

    static CACHE_GEN: AtomicU64 = AtomicU64::new(0);
    static CLOUD_VENDOR_CACHE: Mutex<Option<(u64, Vec<CloudVendorEntry>)>> = Mutex::new(None);

    fn extensions_root() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("extensions")
    }

    fn state_path() -> PathBuf {
        extensions_root().join("state.json")
    }

    fn private_dir(path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    fn write_private_file(path: &Path, contents: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        std::fs::write(path, contents).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn read_state() -> ExtensionsState {
        let path = state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ExtensionsState::default(),
        }
    }

    fn write_state(state: &ExtensionsState) -> Result<(), String> {
        let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
        write_private_file(&state_path(), &format!("{text}\n"))
    }

    /// Host manifest for seeded packs: all files live next to the manifest (no `../../`).
    fn host_seed_manifest() -> ExtensionManifest {
        let mut files = BTreeMap::new();
        for name in [
            "cloud-vendors.json",
            "coding-models.json",
            "execution-agents.json",
            "agent-engines.json",
            "leading-mcp.json",
            "models.catalog.default.json",
            "config.default.json",
            "openrouter-models.json",
            "open-weight-models.json",
            "frontier-models.json",
        ] {
            files.insert(name.to_string(), name.to_string());
        }
        ExtensionManifest {
        id: DEFAULT_PACK_ID.to_string(),
        name: "SUSI Default Extension Pack".into(),
        version: "0.1.0".into(),
        api_version: default_api_version(),
        description: "Auto-seeded vendor opinions. Edit files here or drop additional packs under ~/.susi/extensions/<id>/.".into(),
        capabilities: vec![
            "catalog.cloud_vendors".into(),
            "catalog.coding_models".into(),
            "catalog.mcp".into(),
        ],
        requires: Vec::new(),
        optional: Vec::new(),
        permissions: vec!["filesystem.read".into()],
        files,
        extra: HashMap::new(),
    }
    }

    fn bundled_bytes_for(name: &str) -> Option<&'static str> {
        match name {
            "manifest.json" => Some(BUNDLED_MANIFEST),
            "cloud-vendors.json" => Some(BUNDLED_CLOUD_VENDORS),
            "coding-models.json" => Some(BUNDLED_CODING_MODELS),
            "execution-agents.json" => Some(BUNDLED_EXECUTION_AGENTS),
            "agent-engines.json" => Some(BUNDLED_AGENT_ENGINES),
            "leading-mcp.json" => Some(BUNDLED_LEADING_MCP),
            "models.catalog.default.json" => Some(BUNDLED_MODELS_CATALOG),
            "config.default.json" => Some(BUNDLED_CONFIG_DEFAULT),
            "openrouter-models.json" => Some(BUNDLED_OPENROUTER_MODELS),
            "open-weight-models.json" => Some(BUNDLED_OPEN_WEIGHT_MODELS),
            "frontier-models.json" => Some(BUNDLED_FRONTIER_MODELS),
            _ => None,
        }
    }

    /// Seed the default pack onto the host if missing (idempotent, never overwrites).
    pub fn seed_default_pack() -> Result<PathBuf, String> {
        let root = extensions_root().join(DEFAULT_PACK_ID);
        private_dir(&root)?;
        let host_manifest = host_seed_manifest();
        let manifest_path = root.join("manifest.json");
        if !manifest_path.is_file() {
            let text = serde_json::to_string_pretty(&host_manifest).map_err(|e| e.to_string())?;
            write_private_file(&manifest_path, &format!("{text}\n"))?;
        }
        for name in host_manifest.files.keys() {
            let dest = root.join(name);
            if dest.is_file() {
                continue;
            }
            let Some(bytes) = bundled_bytes_for(name) else {
                continue;
            };
            write_private_file(&dest, bytes)?;
        }
        Ok(root)
    }

    /// Zero-config entry: create extensions root, seed default, ensure state, return active pack.
    pub fn ensure_extensions_substrate() -> Result<ExtensionPack, String> {
        private_dir(&extensions_root())?;
        seed_default_pack()?;
        let mut state = read_state();
        if !state.loaded.iter().any(|id| id == DEFAULT_PACK_ID) {
            state.loaded.push(DEFAULT_PACK_ID.to_string());
        }
        // Discover other pack dirs and auto-load them (zero-config), unless unloaded.
        if let Ok(entries) = std::fs::read_dir(extensions_root()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(id) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if id == "state.json" || id.starts_with('.') {
                    continue;
                }
                if !path.join("manifest.json").is_file() {
                    continue;
                }
                let discovered = manifest_for(id);
                if let Err(e) = validate_manifest(&discovered) {
                    eprintln!(
                        "[extensions] skipping pack `{id}`: {e} (optional failure; core continues)"
                    );
                    continue;
                }
                if state.unloaded.iter().any(|x| x == id) {
                    continue;
                }
                if !state.loaded.iter().any(|x| x == id) {
                    state.loaded.push(id.to_string());
                }
            }
        }
        if state.active.trim().is_empty() {
            state.active = DEFAULT_PACK_ID.to_string();
        }
        // Env override always wins for active id.
        if let Ok(forced) = std::env::var("SUSI_EXTENSION_PACK") {
            let forced = forced.trim();
            if !forced.is_empty() {
                state.active = forced.to_string();
                if !state.loaded.iter().any(|x| x == forced) {
                    state.loaded.push(forced.to_string());
                }
            }
        }
        // If active pack dir is missing (and not default), fall back.
        let active_root = extensions_root().join(&state.active);
        if state.active != DEFAULT_PACK_ID && !active_root.join("manifest.json").is_file() {
            state.active = DEFAULT_PACK_ID.to_string();
        }
        write_state(&state)?;
        Ok(ExtensionPack {
            id: state.active.clone(),
            root: extensions_root().join(&state.active),
        })
    }

    /// Invalidate in-process pack caches after load/unload/seed.
    pub fn invalidate_extension_caches() {
        CACHE_GEN.fetch_add(1, Ordering::SeqCst);
        *CLOUD_VENDOR_CACHE.lock() = None;
    }

    /// List installed / discoverable packs.
    pub fn list_packs() -> Vec<PackStatus> {
        let _ = ensure_extensions_substrate();
        let state = read_state();
        let mut packs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        let mut push_pack = |id: &str| {
            if !seen.insert(id.to_string()) {
                return;
            }
            let root = extensions_root().join(id);
            let manifest = manifest_for(id);
            packs.push(PackStatus {
                id: id.to_string(),
                name: manifest.name,
                version: manifest.version,
                seeded: root.join("manifest.json").is_file(),
                loaded: state.loaded.iter().any(|x| x == id),
                active: state.active == id,
                root,
            });
        };

        push_pack(DEFAULT_PACK_ID);
        for id in &state.loaded {
            push_pack(id);
        }
        if let Ok(entries) = std::fs::read_dir(extensions_root()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(id) = path.file_name().and_then(|s| s.to_str()) {
                        if path.join("manifest.json").is_file() {
                            push_pack(id);
                        }
                    }
                }
            }
        }
        packs.sort_by(|a, b| a.id.cmp(&b.id));
        packs
    }

    /// Create a minimal pack shell under `~/.susi/extensions/<id>/` (idempotent).
    /// Does not activate; call [`load_pack`] to make it active.
    #[allow(clippy::expect_used)]
    pub fn create_pack(id: &str) -> Result<PackStatus, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("pack id must not be empty".into());
        }
        if id.contains('/') || id.contains('\\') || id == "." || id == ".." || id.starts_with('.') {
            return Err(format!("invalid pack id `{id}`"));
        }
        ensure_extensions_substrate()?;
        if id == DEFAULT_PACK_ID {
            seed_default_pack()?;
            // Mandate 42: safe - seed_default_pack() just returned Ok, meaning it
            // wrote (or confirmed) the default pack's manifest on disk; list_packs()
            // reads that same directory synchronously with no intervening mutation,
            // so the just-seeded id is guaranteed to be present in its output.
            return Ok(list_packs()
                .into_iter()
                .find(|p| p.id == DEFAULT_PACK_ID)
                .expect("default pack"));
        }
        let root = extensions_root().join(id);
        private_dir(&root)?;
        let manifest_path = root.join("manifest.json");
        if !manifest_path.is_file() {
            let manifest = ExtensionManifest {
                id: id.to_string(),
                name: id.to_string(),
                version: "0.0.1".into(),
                api_version: default_api_version(),
                description: format!("Host extension pack `{id}`."),
                capabilities: Vec::new(),
                requires: Vec::new(),
                optional: Vec::new(),
                permissions: Vec::new(),
                files: BTreeMap::new(),
                extra: HashMap::new(),
            };
            let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
            write_private_file(&manifest_path, &format!("{text}\n"))?;
        }
        invalidate_extension_caches();
        // Mandate 42: safe - the manifest for `id` was just written above (or
        // already existed), and list_packs() reads that same directory
        // synchronously with no intervening mutation, so `id` is guaranteed to
        // be present in its output.
        Ok(list_packs()
            .into_iter()
            .find(|p| p.id == id)
            .expect("created pack must appear in list"))
    }

    /// Load (activate + mark loaded) a pack. Seeds `default` if needed.
    #[allow(clippy::expect_used)]
    pub fn load_pack(id: &str) -> Result<PackStatus, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("pack id must not be empty".into());
        }
        ensure_extensions_substrate()?;
        if id == DEFAULT_PACK_ID {
            seed_default_pack()?;
        }
        let root = extensions_root().join(id);
        if !root.join("manifest.json").is_file() {
            return Err(format!(
                "pack `{id}` not found under {} — drop a manifest.json there or use `default`",
                extensions_root().display()
            ));
        }
        let manifest = manifest_for(id);
        if let Err(e) = validate_manifest(&manifest) {
            // Optional / third-party packs must not brick the substrate: refuse
            // this pack only, leave active pack unchanged.
            return Err(format!("pack `{id}` rejected: {e}"));
        }
        let mut state = read_state();
        state.unloaded.retain(|x| x != id);
        if !state.loaded.iter().any(|x| x == id) {
            state.loaded.push(id.to_string());
        }
        state.active = id.to_string();
        write_state(&state)?;
        invalidate_extension_caches();
        // Mandate 42: safe - the early return above already guarantees
        // `root.join("manifest.json")` exists for `id` before this point, and
        // list_packs() reads that same directory synchronously with no
        // intervening mutation, so `id` is guaranteed to be present in its output.
        Ok(list_packs()
            .into_iter()
            .find(|p| p.id == id)
            .expect("loaded pack must appear in list"))
    }

    /// Unload a pack (deactivate; files kept). Cannot unload the last remaining pack —
    /// falls back to seeded `default` still loaded.
    pub fn unload_pack(id: &str) -> Result<PackStatus, String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("pack id must not be empty".into());
        }
        ensure_extensions_substrate()?;
        let mut state = read_state();
        state.loaded.retain(|x| x != id);
        if !state.unloaded.iter().any(|x| x == id) {
            state.unloaded.push(id.to_string());
        }
        if state.loaded.is_empty() {
            seed_default_pack()?;
            state.loaded.push(DEFAULT_PACK_ID.to_string());
            state.unloaded.retain(|x| x != DEFAULT_PACK_ID);
        }
        if state.active == id {
            state.active = state
                .loaded
                .iter()
                .find(|x| x.as_str() == DEFAULT_PACK_ID)
                .cloned()
                .unwrap_or_else(|| state.loaded[0].clone());
        }
        write_state(&state)?;
        invalidate_extension_caches();
        Ok(PackStatus {
            id: id.to_string(),
            name: id.to_string(),
            version: String::new(),
            seeded: extensions_root().join(id).join("manifest.json").is_file(),
            loaded: false,
            active: false,
            root: extensions_root().join(id),
        })
    }

    /// Resolve the active pack (auto-seeds substrate first).
    pub fn active_pack() -> ExtensionPack {
        match ensure_extensions_substrate() {
            Ok(pack) => {
                if let Ok(forced) = std::env::var("SUSI_EXTENSION_PACK") {
                    let forced = forced.trim();
                    if !forced.is_empty() {
                        return ExtensionPack {
                            id: forced.to_string(),
                            root: extensions_root().join(forced),
                        };
                    }
                }
                pack
            }
            Err(_) => ExtensionPack {
                id: DEFAULT_PACK_ID.to_string(),
                root: extensions_root().join(DEFAULT_PACK_ID),
            },
        }
    }

    /// Host pack file path when present and readable as a file.
    pub fn pack_file(name: &str) -> Option<PathBuf> {
        let _ = ensure_extensions_substrate();
        let pack = active_pack();
        resolve_pack_path(&pack, name)
    }

    fn resolve_pack_path(pack: &ExtensionPack, name: &str) -> Option<PathBuf> {
        // Runtime jail (defense in depth with validate_manifest): logical names and
        // `files` targets stay inside the pack root unless the manifest declares
        // `filesystem.read`.
        if !path_escapes_pack_root(name) {
            let direct = pack.root.join(name);
            if direct.is_file() {
                return Some(direct);
            }
        }
        let manifest = manifest_for(&pack.id);
        if let Some(rel) = manifest.files.get(name).map(|s| s.as_str()) {
            let escapes = path_escapes_pack_root(rel);
            let can_read_outside = manifest.permissions.iter().any(|p| p == "filesystem.read");
            if Path::new(rel).is_absolute() || (escapes && !can_read_outside) {
                eprintln!(
                "[extensions] pack `{}` denied `{name}` (`{rel}` escapes pack root without `filesystem.read`)",
                pack.id
            );
                return None;
            }
            let mapped = pack.root.join(rel);
            if mapped.is_file() {
                return Some(mapped);
            }
        }
        None
    }

    /// Load JSON from the host pack file when present; otherwise parse `bundled`.
    #[allow(clippy::panic)]
    pub fn load_json_or_bundled<T: DeserializeOwned>(pack_relative: &str, bundled: &str) -> T {
        let _ = ensure_extensions_substrate();
        if let Some(path) = pack_file(pack_relative) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(value) = serde_json::from_str(&text) {
                    return value;
                }
            }
        }
        // Mandate 42: safe - `bundled` is always a `&'static str` produced by an
        // include_str! at the call site (compiled into the binary), not a
        // user-editable runtime file, same pattern as sandbox/manager.rs's
        // bundled-default `.expect()` calls. Parsing either always succeeds or
        // always fails for a given binary - a failure is a build/packaging bug
        // caught by any test run, never a runtime condition that varies between
        // calls.
        serde_json::from_str(bundled).unwrap_or_else(|e| {
            panic!("bundled extension pack file `{pack_relative}` must be valid JSON: {e}")
        })
    }

    /// Bundled default-pack manifest (compile-time source tree layout).
    #[allow(clippy::expect_used)]
    pub fn bundled_manifest() -> ExtensionManifest {
        // Mandate 42: safe - BUNDLED_MANIFEST is compiled in via include_str!,
        // see the comment on load_json_or_bundled above.
        serde_json::from_str(BUNDLED_MANIFEST)
            .expect("bundled config/extensions/default/manifest.json must be valid JSON")
    }

    /// Manifest for a pack id: host file wins, else bundled default when id is `default`.
    pub fn manifest_for(pack_id: &str) -> ExtensionManifest {
        let root = extensions_root().join(pack_id);
        let host = root.join("manifest.json");
        if host.is_file() {
            if let Ok(text) = std::fs::read_to_string(&host) {
                if let Ok(m) = serde_json::from_str::<ExtensionManifest>(&text) {
                    return m;
                }
            }
        }
        if pack_id == DEFAULT_PACK_ID {
            return bundled_manifest();
        }
        ExtensionManifest {
            id: pack_id.to_string(),
            name: pack_id.to_string(),
            version: "0.0.0".into(),
            api_version: default_api_version(),
            description: String::new(),
            capabilities: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
            permissions: Vec::new(),
            files: Default::default(),
            extra: HashMap::new(),
        }
    }

    /// Cloud vendors from the active pack (host override or bundled default).
    pub fn load_cloud_vendors() -> Vec<CloudVendorEntry> {
        let _ = ensure_extensions_substrate();
        let generation = CACHE_GEN.load(Ordering::SeqCst);
        {
            let cache = CLOUD_VENDOR_CACHE.lock();
            if let Some((cached_gen, vendors)) = cache.as_ref() {
                if *cached_gen == generation {
                    return vendors.clone();
                }
            }
        }
        let vendors: Vec<CloudVendorEntry> =
            load_json_or_bundled("cloud-vendors.json", BUNDLED_CLOUD_VENDORS);
        *CLOUD_VENDOR_CACHE.lock() = Some((generation, vendors.clone()));
        vendors
    }
}
mod config {
    use crate::susi_error::EaiResult;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::susi_config::json_util::{
        atomic_write_json_pretty, merge_missing_registry_defaults, DynamicRegistry,
    };
    use crate::susi_config::types::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SusiConfig {
        #[serde(flatten)]
        pub settings: DynamicRegistry, // ALL settings are dynamic, loaded from config.default.json
    }

    impl Default for SusiConfig {
        fn default() -> Self {
            Self::bundled_defaults().clone()
        }
    }

    /// Host-contract ports live in `crate::susi_paths::ports` (compile-time). Bundled
    /// JSON may still list them for documentation; they must not be backfilled
    /// into `~/.susi/config.json` on heal.
    const HOST_CONTRACT_PORT_KEYS: &[&str] = &[
        "gmcp_port",
        "gmcp_http_port",
        "gemi_port",
        "udp_discovery_port",
    ];

    impl SusiConfig {
        #[allow(clippy::expect_used)]
        fn bundled_defaults() -> &'static Self {
            static DEFAULTS: std::sync::OnceLock<SusiConfig> = std::sync::OnceLock::new();
            DEFAULTS.get_or_init(|| {
                serde_json::from_str(include_str!("../../../config/config.default.json"))
                    .expect("bundled config.default.json must be valid JSON")
            })
        }

        /// Bundled defaults with host-contract port keys removed — used only for
        /// heal merges so polluted/legacy port fields are never re-persisted.
        fn heal_defaults() -> &'static DynamicRegistry {
            static DEFAULTS: std::sync::OnceLock<DynamicRegistry> = std::sync::OnceLock::new();
            DEFAULTS.get_or_init(|| {
                let mut settings = Self::bundled_defaults().settings.clone();
                for key in HOST_CONTRACT_PORT_KEYS {
                    settings.remove(*key);
                }
                settings
            })
        }

        fn heal_in_place(cfg: &mut Self) -> bool {
            let mut changed =
                merge_missing_registry_defaults(&mut cfg.settings, Self::heal_defaults());
            // Bearer lives only in ~/.susi/api_token (0600). Never heal or
            // persist it into world-readable config.json.
            if cfg.settings.remove("api_auth_token").is_some() {
                changed = true;
            }
            changed
        }

        fn store() -> &'static crate::susi_config::versioned_store::VersionedJsonStore<Self> {
            static STORE: std::sync::OnceLock<
                crate::susi_config::versioned_store::VersionedJsonStore<SusiConfig>,
            > = std::sync::OnceLock::new();
            STORE.get_or_init(crate::susi_config::versioned_store::VersionedJsonStore::new)
        }

        pub fn get_config_path(global_dir: &Path) -> PathBuf {
            global_dir.join("config.json")
        }

        /// Loads a user's persisted config.json and self-heals schema drift against
        /// the binary's bundled config.default.json: any key (at any nesting depth,
        /// including per-element within same-length arrays like model_ladder) that
        /// exists in the bundled default but is missing from the user's file is
        /// backfilled in memory and the merged result is written back to disk.
        /// Values the user already set are never touched. Without this, a fix that
        /// only lands in config.default.json (e.g. a new field on an existing key)
        /// silently never reaches an install whose config.json predates it.
        pub fn load(global_dir: &Path) -> EaiResult<Self> {
            Ok((*Self::load_arc(global_dir)?).clone())
        }

        /// Shared snapshot of the config (cache-hit is `Arc::clone`, not a deep copy).
        pub fn load_arc(global_dir: &Path) -> EaiResult<std::sync::Arc<Self>> {
            let path = Self::get_config_path(global_dir);
            Self::store().load_arc_with_healing(
                &path,
                || Ok(Self::default()),
                Self::heal_in_place,
                true,
            )
        }

        pub fn reload(global_dir: &Path) -> EaiResult<Self> {
            Ok((*Self::reload_arc(global_dir)?).clone())
        }

        pub fn reload_arc(global_dir: &Path) -> EaiResult<std::sync::Arc<Self>> {
            Self::store().reload_arc_with_healing(
                &Self::get_config_path(global_dir),
                || Ok(Self::default()),
                Self::heal_in_place,
                true,
            )
        }

        pub fn load_global() -> EaiResult<Self> {
            if let Some(cfg) = crate::susi_config::service::get_global() {
                return Ok(cfg);
            }
            Self::load(&crate::susi_paths::SusiDirs::config_dir())
        }

        /// Process-global config snapshot shared across hot request paths.
        pub fn load_global_arc() -> EaiResult<std::sync::Arc<Self>> {
            if let Some(cfg) = crate::susi_config::service::get_global() {
                return Ok(std::sync::Arc::new(cfg));
            }
            Self::load_arc(&crate::susi_paths::SusiDirs::config_dir())
        }

        /// Writes via a same-directory temp file + rename rather than a direct
        /// fs::write (which truncates before writing), so a concurrent reader —
        /// another process's CLI invocation, the daemon's own background cycle —
        /// never observes a torn/empty file. This matters more now that `load()`
        /// can itself trigger a save on schema-drift backfill, making concurrent
        /// writers to the same config.json from multiple processes routine rather
        /// than rare.
        pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
            fs::create_dir_all(global_dir)?;
            if global_dir == crate::susi_paths::SusiDirs::config_dir()
                && crate::susi_config::service::save_global(self)
            {
                return Ok(());
            }
            atomic_write_json_pretty(&Self::get_config_path(global_dir), self)
        }

        // === TYPED ACCESSORS - No hardcoded fields, dynamic getters with defaults ===
        pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
            let v = self.settings.get(key)?;
            T::deserialize(v).ok()
        }

        /// Falls back to the bundled config.default.json's value for `key` (not a
        /// zero-value literal duplicated in Rust) when a user's ~/.susi/config.json
        /// predates this key or omits it. This is the *only* fallback path for every
        /// accessor below — config.default.json is the single source of truth for
        /// every default; there is no second, Rust-side copy of any value that
        /// could silently drift out of sync with it (as several of these already
        /// had: gmcp_http_port/gemi_port/udp_discovery_port were scrambled between
        /// their Rust literal and config.default.json, max_stdin_size_bytes was off
        /// by 100x, reflex_training_threshold by 10x, and alpha_weights_url/
        /// mcp_registry_url's Rust fallback was an empty string).
        fn get_or_bundled_default<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
            self.get(key)
                .unwrap_or_else(|| Self::bundled_defaults().get(key).unwrap_or_default())
        }

        // Public substrate ports are a hard contract with external clients.
        // Accessors ignore any polluted ~/.susi/config.json values (legacy
        // randomization) and always return the canonical ports.
        pub fn gmcp_port(&self) -> u16 {
            let _ = self; // keep method signature; value is not user-overridable
            crate::susi_paths::ports::GMCP
        }
        pub fn gmcp_http_port(&self) -> u16 {
            let _ = self;
            crate::susi_paths::ports::GMCP_HTTP
        }
        pub fn gemi_port(&self) -> u16 {
            let _ = self;
            crate::susi_paths::ports::GEMI
        }
        pub fn udp_discovery_port(&self) -> u16 {
            let _ = self;
            crate::susi_paths::ports::UDP_DISCOVERY
        }
        pub fn execution_lease_secs(&self) -> u64 {
            self.get_or_bundled_default("execution_lease_secs")
        }
        pub fn max_concurrent_agents(&self) -> usize {
            self.get_or_bundled_default("max_concurrent_agents")
        }
        pub fn trust_level(&self) -> String {
            self.get_or_bundled_default("trust_level")
        }
        pub fn max_stdin_size_bytes(&self) -> usize {
            self.get_or_bundled_default("max_stdin_size_bytes")
        }
        pub fn max_rpc_body_bytes(&self) -> usize {
            self.get_or_bundled_default("max_rpc_body_bytes")
        }
        pub fn allow_origin(&self) -> String {
            self.get_or_bundled_default("allow_origin")
        }
        /// Bearer token required on world-facing HTTP surfaces (GMCP HTTP, GEMI
        /// REST). Prefer the dedicated `~/.susi/api_token` file (0600); fall back to
        /// a legacy `settings.api_auth_token` value only for migration.
        pub fn api_auth_token(&self) -> String {
            let token_path = crate::susi_paths::SusiDirs::config_dir().join("api_token");
            if let Ok(from_file) = fs::read_to_string(&token_path) {
                let trimmed = from_file.trim().to_string();
                if !trimmed.is_empty() {
                    return trimmed;
                }
            }
            self.get_or_bundled_default("api_auth_token")
        }

        /// Ensure `api_auth_token` is non-empty on the host substrate. Generates a
        /// 32-byte hex secret on first run, persists it **only** to
        /// `~/.susi/api_token` (0600) — never into world-readable `config.json`.
        pub fn ensure_api_auth_token_seeded() -> String {
            let global_dir = crate::susi_paths::SusiDirs::config_dir();
            let token_path = global_dir.join("api_token");
            if let Ok(existing) = fs::read_to_string(&token_path) {
                let trimmed = existing.trim().to_string();
                if !trimmed.is_empty() {
                    return trimmed;
                }
            }
            // Migrate a legacy config.json copy into the private file, then strip it.
            let mut cfg = Self::load_global().unwrap_or_default();
            let legacy = cfg
                .settings
                .get("api_auth_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let token = if !legacy.is_empty() {
                legacy
            } else {
                let mut raw = [0u8; 32];
                if getrandom::fill(&mut raw).is_err() {
                    // Extremely unlikely; fall back to a process-unique but weaker seed.
                    let fallback = format!(
                        "{}:{}:{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0),
                        global_dir.display()
                    );
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(fallback.as_bytes());
                    raw.copy_from_slice(&digest[..32]);
                }
                hex::encode(raw)
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                let _ = fs::create_dir_all(&global_dir);
                let _ = fs::set_permissions(&global_dir, fs::Permissions::from_mode(0o700));
                // Create with 0600 atomically — never write-then-chmod (umask window).
                let mut options = fs::OpenOptions::new();
                options.write(true).create(true).truncate(true).mode(0o600);
                match options.open(&token_path).and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(token.as_bytes())?;
                    f.sync_all()
                }) {
                    Ok(()) => {
                        let _ = fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600));
                    }
                    Err(_) => {
                        let _ = fs::write(&token_path, &token);
                        let _ = fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600));
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = fs::write(&token_path, &token);
            }
            if cfg.settings.remove("api_auth_token").is_some() {
                let _ = cfg.save(&global_dir);
            }
            eprintln!(
                "[Zero-Trust] Seeded host API bearer token → {} (required on HTTP 9090/9091/9093)",
                token_path.display()
            );
            token
        }

        /// Max requests per IP per 60s window on world-facing HTTP surfaces. 0
        /// disables rate limiting.
        pub fn rate_limit_per_minute(&self) -> u32 {
            self.get_or_bundled_default("rate_limit_per_minute")
        }

        pub fn default_model(&self) -> String {
            self.get_or_bundled_default("default_model")
        }
        pub fn default_engine(&self) -> String {
            self.get_or_bundled_default("default_engine")
        }
        pub fn mcp_registry_url(&self) -> String {
            self.get_or_bundled_default("mcp_registry_url")
        }
        pub fn bootstrap_mcp_servers<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
            self.get("bootstrap_mcp_servers").unwrap_or_default()
        }
        pub fn local_scan_paths(&self) -> Vec<String> {
            self.get("local_scan_paths").unwrap_or_default()
        }
        pub fn discoverable_assets<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
            self.get("discoverable_assets").unwrap_or_default()
        }
        pub fn governance(&self) -> GovernancePatterns {
            self.get_or_bundled_default("governance")
        }
        pub fn admin_pulses(&self) -> AdminPulsesConfig {
            self.get_or_bundled_default("admin_pulses")
        }
        pub fn intent_classify(&self) -> IntentClassifyConfig {
            self.get_or_bundled_default("intent_classify")
        }
        pub fn alpha_weights_url(&self) -> String {
            self.get_or_bundled_default("alpha_weights_url")
        }
        pub fn alpha_weights_filename(&self) -> String {
            self.get_or_bundled_default("alpha_weights_filename")
        }
        pub fn tokenizer_filename(&self) -> String {
            self.get_or_bundled_default("tokenizer_filename")
        }
        pub fn hf_base_url(&self) -> String {
            self.get_or_bundled_default("hf_base_url")
        }
        pub fn qdrant_url(&self) -> String {
            self.get_or_bundled_default("qdrant_url")
        }
        pub fn crates_io_api_url(&self) -> String {
            self.get_or_bundled_default("crates_io_api_url")
        }
        pub fn inference_endpoints(&self) -> InferenceEndpointsConfig {
            self.get_or_bundled_default("inference_endpoints")
        }
        pub fn agent_rank_threshold(&self) -> f32 {
            self.get_or_bundled_default("agent_rank_threshold")
        }
        pub fn cloud_scout_timeout_secs(&self) -> u64 {
            self.get_or_bundled_default("cloud_scout_timeout_secs")
        }
        pub fn model_provisioning_wait_secs(&self) -> u64 {
            self.get_or_bundled_default("model_provisioning_wait_secs")
        }
        /// Interval for re-probing local inference engines and MCP tools after
        /// daemon start. `0` disables the background rediscovery loop.
        pub fn capability_rediscovery_secs(&self) -> u64 {
            self.get_or_bundled_default("capability_rediscovery_secs")
        }
        pub fn reflex_training_threshold(&self) -> usize {
            self.get_or_bundled_default("reflex_training_threshold")
        }
        pub fn model_lifecycle(&self) -> ModelLifecycleConfig {
            self.get_or_bundled_default("model_lifecycle")
        }
        pub fn inference_routing(&self) -> InferenceRoutingConfig {
            self.get_or_bundled_default("inference_routing")
        }

        pub fn privacy(&self) -> PrivacyConfig {
            self.get_or_bundled_default("privacy")
        }

        /// Leading external coding agents mounted as pluggable swarm peers.
        ///
        /// `config.default.json` / host `config.json` may list CLI/HTTP/A2A peers and
        /// optional managed overlays. **Managed** peers are also admitted from the
        /// execution-agents + agent-engines catalogs (`peer_name`) so adding a
        /// catalog entry does not require duplicating it under `external_peer_agents`.
        pub fn external_peer_agents(&self) -> Vec<ExternalPeerAgentSpec> {
            let mut peers: Vec<ExternalPeerAgentSpec> =
                self.get_or_bundled_default("external_peer_agents");
            // Migrate only exact historical built-ins. Preserve user-edited commands,
            // endpoints, argv, credentials and timeouts. New bundled peers are admitted
            // for existing installations as well as fresh installs.
            let legacy: Vec<ExternalPeerAgentSpec> =
                serde_json::from_str(include_str!("../../../config/execution-peers.legacy.json"))
                    .unwrap_or_default();
            let defaults: Vec<ExternalPeerAgentSpec> =
                Self::default().get_or_bundled_default("external_peer_agents");
            for peer in &mut peers {
                if legacy.iter().any(|old| peer_exact_legacy_match(old, peer)) {
                    if let Some(updated) = defaults.iter().find(|p| p.name == peer.name) {
                        *peer = updated.clone();
                    }
                }
            }
            for default in defaults.into_iter().filter(|p| p.protocol == "managed") {
                if !peers.iter().any(|p| p.name == default.name) {
                    peers.push(default);
                }
            }
            for catalog_peer in managed_peers_from_catalogs() {
                if !peers.iter().any(|p| p.name == catalog_peer.name) {
                    peers.push(catalog_peer);
                }
            }
            peers
        }

        /// Leading models catalog (~50 curated); live `/models` discovery remains
        /// unbounded. Override via config key `model_catalog`.
        #[allow(clippy::expect_used)]
        pub fn model_catalog(&self) -> Vec<ModelCatalogEntry> {
            if let Some(cfg) = self.get::<ModelCatalogConfig>("model_catalog") {
                if !cfg.models.is_empty() {
                    return cfg.models;
                }
            }
            static BUNDLED: std::sync::OnceLock<Vec<ModelCatalogEntry>> =
                std::sync::OnceLock::new();
            BUNDLED
                .get_or_init(|| {
                    let file: ModelCatalogConfig = serde_json::from_str(include_str!(
                        "../../../config/models.catalog.default.json"
                    ))
                    .expect("bundled models.catalog.default.json must be valid");
                    file.models
                })
                .clone()
        }

        /// Bundled leading MCP scout registry (~100 real packages). Remote scout
        /// can refresh `global_mcp_registry.json`; not all are hot-plugged at once.
        pub fn leading_mcp_registry_json() -> &'static str {
            include_str!("../../../config/mcp.registry.default.json")
        }

        /// The explicitly configured ladder, or empty if none is set. This is a
        /// pure config accessor with no network/discovery fallback — `sandbox`
        /// must not depend on `gemi::hf_discovery` for that (it would create a
        /// cycle, since `gemi` already depends on `sandbox` for `SusiConfig`
        /// itself). Callers that want the dynamic-discovery fallback when this
        /// is empty should use `gemi::hf_discovery::resolve_model_ladder`.
        pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
            self.get_or_bundled_default("model_ladder")
        }
        pub fn default_fallback_model(&self) -> ModelLadderConfigStep {
            self.get_or_bundled_default("default_fallback_model")
        }
        pub fn admin_command_routing(&self) -> HashMap<String, Vec<Vec<String>>> {
            self.get_or_bundled_default("admin_command_routing")
        }
        pub fn agent_routing(&self) -> HashMap<String, Vec<String>> {
            self.get_or_bundled_default("agent_routing")
        }
        pub fn model_scoring_heuristics(&self) -> ModelScoringHeuristics {
            self.get_or_bundled_default("model_scoring_heuristics")
        }
        pub fn memory_experience_heuristics(&self) -> MemoryExperienceHeuristics {
            self.get_or_bundled_default("memory_experience_heuristics")
        }
        pub fn sandbox_image(&self) -> String {
            self.get_or_bundled_default("sandbox_image")
        }
        /// Special-token IDs that terminate generation (previously hardcoded as
        /// `1 | 2 | 32000 | 151643` directly in the inference loop — vendor/
        /// tokenizer-specific magic numbers with zero comment on which model
        /// family each belonged to).
        pub fn eos_token_ids(&self) -> Vec<u32> {
            self.get_or_bundled_default("eos_token_ids")
        }
        /// Maximum simultaneous HTTP completion jobs; excess clients receive HTTP 429.
        pub fn gemi_max_concurrent_requests(&self) -> usize {
            self.get_or_bundled_default::<usize>("gemi_max_concurrent_requests")
                .max(1)
        }
        pub fn max_generation_tokens(&self) -> usize {
            self.get_or_bundled_default("max_generation_tokens")
        }
        /// Penalty divisor applied to already-seen tokens' logits before argmax
        /// (llama.cpp convention: >1.0 discourages repetition, 1.0 disables it).
        /// Pure greedy decoding with no penalty readily loops on tiny models -
        /// observed live on qwen2.5-0.5b as a "Name three colors" response
        /// degenerating into an infinitely-nesting repeated JSON structure.
        pub fn repeat_penalty(&self) -> f32 {
            self.get_or_bundled_default("repeat_penalty")
        }
        /// How many of the most recent tokens (prompt + generated) count toward
        /// the repeat penalty above.
        pub fn repeat_last_n(&self) -> usize {
            self.get_or_bundled_default("repeat_last_n")
        }
        /// Whether the generation loop may draft-and-verify multiple tokens per
        /// target-model forward pass (see `gemi::speculative`) instead of one
        /// token at a time. Verification always falls back to the target
        /// model's own greedy choice on the first disagreement, so the emitted
        /// token sequence is provably identical to plain greedy decoding
        /// either way (see `speculative::tests::
        /// test_speculative_output_matches_plain_greedy_decoding`) - this only
        /// gates whether the batched path is attempted, never behavior.
        ///
        /// Defaults to `false`: measured live on this host (RTX 2000 Ada
        /// Laptop, 8GB VRAM), a fully GPU-resident Qwen2.5-7B target with a
        /// 0.5B draft ran at 4.36 tok/s versus 18.47 tok/s for plain greedy
        /// decoding of the same model - a 4x regression, not the hoped-for
        /// speedup. CUDA's quantized matmul genuinely gets cheaper per-token
        /// with a wider batch (confirmed via `cudarc`'s `fast_mmq` threshold),
        /// but that saving is consumed by the extra host/kernel-launch round
        /// trips this adds: the draft model still needs `speculative_draft_tokens
        /// - 1` *sequential* single-token forwards per round (autoregressive,
        ///   can't be batched), plus a resync forward, on top of the target's
        ///   batched verify call - more total round trips than the classic loop
        ///   for the same tokens, and per-call host/launch overhead dominates
        ///   over raw compute at these model sizes on this stack. Left
        ///   configurable (and the implementation fully correctness-tested) in
        ///   case a future candle version, different hardware, or a larger
        ///   draft_chunk changes this trade-off - but never default-on without
        ///   remeasuring.
        pub fn speculative_decoding_enabled(&self) -> bool {
            self.get_or_bundled_default("speculative_decoding_enabled")
        }
        /// How many tokens the draft model proposes ahead of the target model
        /// per verification round. Larger values amortize more work into each
        /// batched target-model forward pass (raising GPU utilization) but waste
        /// more of that pass whenever the draft diverges early.
        pub fn speculative_draft_tokens(&self) -> usize {
            self.get_or_bundled_default("speculative_draft_tokens")
        }
        /// Upper bound (prompt + generated tokens combined) the native Qwen2
        /// engine's KV cache preallocates per layer at model-load time (see
        /// `qwen2_split::ModelWeights::from_gguf_split`). The cache is written
        /// into in place as generation proceeds rather than reallocated every
        /// token, so this must be decided once, before any particular
        /// request's prompt length is known - sized generously above
        /// `max_generation_tokens` to comfortably cover realistic prompts.
        /// Exceeding it mid-generation is a hard error (not silent truncation
        /// or a fallback to reallocation): raise this value if it's ever hit.
        pub fn kv_cache_capacity_tokens(&self) -> usize {
            self.get_or_bundled_default("kv_cache_capacity_tokens")
        }
        /// Risk substrings that block reasoning OUTPUT before it's returned as a
        /// final answer — a distinct security layer from `governance()`'s
        /// `destructive_commands` (which gates COMMANDS before execution); the
        /// two lists overlap in spirit but not in content.
        pub fn axiomatic_risk_patterns(&self) -> Vec<String> {
            self.get_or_bundled_default("axiomatic_risk_patterns")
        }
        pub fn model_scan_exclude_dirs(&self) -> Vec<String> {
            self.get_or_bundled_default("model_scan_exclude_dirs")
        }
        /// Deliberately narrower than `model_scan_exclude_dirs`: this gates
        /// `recursive_scan_model_dir_for_paths`, which `deep_scan_home_and_register`
        /// invokes *with* `.cache`/`.local` etc. as starting directories (they're
        /// dot-prefixed, so a separate first pass has to seed them explicitly) — if
        /// this list excluded them too, the scan would refuse to look inside its
        /// own seeded roots.
        pub fn model_discovery_exclude_dirs(&self) -> Vec<String> {
            self.get_or_bundled_default("model_discovery_exclude_dirs")
        }
        pub fn home_scan_root_exclude_dirs(&self) -> Vec<String> {
            self.get_or_bundled_default("home_scan_root_exclude_dirs")
        }
        pub fn model_file_extensions(&self) -> Vec<String> {
            self.get_or_bundled_default("model_file_extensions")
        }
        pub fn model_file_min_bytes(&self) -> u64 {
            self.get_or_bundled_default("model_file_min_bytes")
        }
    }

    /// Exact field equality for legacy peer migrate (avoids double `serde_json::to_value`).
    fn peer_exact_legacy_match(a: &ExternalPeerAgentSpec, b: &ExternalPeerAgentSpec) -> bool {
        a.name == b.name
            && a.description == b.description
            && a.protocol == b.protocol
            && a.api_base == b.api_base
            && a.model == b.model
            && a.detect_bins == b.detect_bins
            && a.command == b.command
            && a.args == b.args
            && a.timeout_secs == b.timeout_secs
            && a.api_key_env == b.api_key_env
    }

    /// Minimal catalog row used to admit managed swarm peers from extension packs.
    #[derive(Debug, Deserialize)]
    struct CatalogPeerHint {
        peer_name: String,
        name: String,
        #[serde(flatten, default)]
        _extra: HashMap<String, serde_json::Value>,
    }

    fn managed_peers_from_catalogs() -> Vec<ExternalPeerAgentSpec> {
        let mut out = Vec::new();
        let catalogs = [
            (
                "execution-agents.json",
                include_str!("../../../config/execution-agents.json"),
            ),
            (
                "agent-engines.json",
                include_str!("../../../config/agent-engines.json"),
            ),
        ];
        for (logical, bundled) in catalogs {
            let entries: Vec<CatalogPeerHint> =
                crate::susi_config::extensions::load_json_or_bundled(logical, bundled);
            for entry in entries {
                let peer = entry.peer_name.trim();
                if peer.is_empty() {
                    continue;
                }
                out.push(ExternalPeerAgentSpec {
                    name: peer.to_string(),
                    description: format!(
                        "{} managed executor/framework; setup via susi agents / susi frameworks.",
                        entry.name
                    ),
                    protocol: "managed".into(),
                    ..Default::default()
                });
            }
        }
        out
    }

    // Compatibility shim for old code that accessed fields directly
    impl std::ops::Deref for SusiConfig {
        type Target = DynamicRegistry;
        fn deref(&self) -> &Self::Target {
            &self.settings
        }
    }
}

pub use config::SusiConfig;
pub use json_util::{
    atomic_write_json_pretty, confined_workspace_join, http_agent, merge_missing_json_defaults,
    merge_missing_registry_defaults, DynamicRegistry, DynamicValue, ModelTier, ProviderType,
    StringRegistry,
};
pub use types::*;
pub use versioned_store::VersionedJsonStore;
