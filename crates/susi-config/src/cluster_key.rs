//! Shared cluster key (`~/.susi/cluster.key`, 0600) and the signed LAN
//! discovery handshake behind authenticated peer membership (VC-200-001).
//!
//! Protocol (`susi-peer-v1`):
//! - Scout sends
//!   `SUSI_PING_SIG:<node_id>:<caps_csv>:<checksum>:<bloom_hex>:<nonce>:<mac>`
//!   where `node_id` is the sender's persisted identity (`node_id()`) and
//!   `mac` is HMAC-SHA256 over `susi-peer-v1:ping:<same fields>`. Pre-identity
//!   6-field pings (no `node_id`) still verify with an empty sender id so
//!   mixed-version clusters handshake; `peers add` falls back to that format
//!   for daemons that predate the field.
//! - The daemon verifies the MAC and answers
//!   `SUSI_PONG_SIG:<node_id>:<checksum>:<bloom_hex>:<nonce>:<roster_hex>:<mac>`
//!   — the nonce echoed back binds the pong to this exchange, so replaying a
//!   captured signed pong against a different ping fails verification. The
//!   `roster_hex` field carries the responder's verified members
//!   (`node_id@address;…`, hex-encoded, ≤8 entries) so one handshake teaches
//!   the joiner the whole cluster; pre-gossip 6-field pongs still verify.
//!
//! Peers that only ever produce unsigned pongs stay `Discovered` — a spoofed
//! UDP answer can never reach `Explicit` without the shared key. The operator
//! copies `cluster.key` to every node that should join the cluster.

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
        return hex::encode(Sha256::digest(seed.as_bytes())[..16].as_ref());
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

/// Verified fields of a signed ping: `(node_id, caps_csv, checksum,
/// bloom_hex, nonce)` — `node_id` is empty for pre-identity senders.
pub type VerifiedPing = (String, String, u64, String, String);

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
/// still handshake.
pub fn verify_signed_ping(msg: &str) -> Option<VerifiedPing> {
    let key = cluster_key()?;
    let parts: Vec<&str> = msg.split(':').collect();
    if parts.first() != Some(&"SUSI_PING_SIG") {
        return None;
    }
    // SUSI_PING_SIG:[<node_id>:]<caps>:<checksum>:<bloom>:<nonce>:<mac>
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
    let expected = if parts.len() == 6 {
        mac_tag(&key, &["ping", caps, checksum_s, bloom, nonce])
    } else {
        mac_tag(&key, &["ping", node_id, caps, checksum_s, bloom, nonce])
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

/// Verified fields of a signed pong: `(node_id, checksum, bloom_hex,
/// roster)`.
pub type VerifiedPong = (String, u64, String, Vec<(String, String)>);

/// Verify a signed pong against `expected_nonce`. `None` for
/// malformed input, a bad MAC, or a nonce that does not match the
/// nonce sent with our ping. Legacy 6-field pongs (pre-gossip
/// builds) verify with an empty roster so mixed-version clusters
/// still handshake.
pub fn verify_signed_pong(msg: &str, expected_nonce: &str) -> Option<VerifiedPong> {
    let key = cluster_key()?;
    let parts: Vec<&str> = msg.split(':').collect();
    if parts.first() != Some(&"SUSI_PONG_SIG") {
        return None;
    }
    // SUSI_PONG_SIG:<node_id>:<checksum>:<bloom>:<nonce>:[<roster>:]<mac>
    let (node_id, checksum_s, bloom, nonce, roster_hex, mac) = match parts.len() {
        7 => (parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]),
        6 => (parts[1], parts[2], parts[3], parts[4], "0", parts[5]),
        _ => return None,
    };
    for f in [node_id, checksum_s, bloom, nonce, roster_hex] {
        if !wire_safe(f) {
            return None;
        }
    }
    if nonce != expected_nonce {
        return None;
    }
    let expected = if parts.len() == 6 {
        mac_tag(&key, &["pong", node_id, checksum_s, bloom, nonce])
    } else {
        mac_tag(
            &key,
            &["pong", node_id, checksum_s, bloom, nonce, roster_hex],
        )
    };
    if expected != mac {
        return None;
    }
    Some((
        node_id.to_string(),
        checksum_s.parse().unwrap_or(0),
        bloom.to_string(),
        decode_roster(roster_hex),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_key_env() -> TempHomeGuard<'static> {
        // Serialize env mutation within this test binary.
        let guard = crate::env_test_lock();
        let tmp = std::env::temp_dir().join(format!(
            "susi_ck_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Pre-create `.susi` so SusiDirs picks the deterministic legacy path.
        let _ = std::fs::create_dir_all(tmp.join(".susi"));
        unsafe {
            std::env::set_var("HOME", &tmp);
            std::env::set_var("USERPROFILE", &tmp);
            std::env::set_var("XDG_CONFIG_HOME", tmp.join("xdg"));
        }
        TempHomeGuard { _guard: guard, tmp }
    }

    struct TempHomeGuard<'a> {
        _guard: std::sync::MutexGuard<'a, ()>,
        tmp: PathBuf,
    }
    impl Drop for TempHomeGuard<'_> {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = std::fs::remove_dir_all(&self.tmp);
        }
    }

    #[test]
    fn signed_ping_pong_roundtrip_verifies() {
        let _t = set_key_env();
        let (ping, nonce) = signed_ping("CORE,GPU", 7, "00ff").expect("signed ping");
        let (pinger_id, caps, checksum, bloom, echoed) =
            verify_signed_ping(&ping).expect("verify ping");
        assert!(pinger_id.starts_with("susi-node-"));
        assert_eq!(
            (caps.as_str(), checksum, bloom.as_str()),
            ("CORE,GPU", 7, "00ff")
        );
        assert_eq!(echoed, nonce);
        let roster = vec![
            ("peer-b".to_string(), "10.0.0.2:9090".to_string()),
            ("peer-c".to_string(), "10.0.0.3:9090".to_string()),
        ];
        let pong =
            signed_pong("susi-daemon-node", 0, "abcd", &echoed, &roster).expect("signed pong");
        let (node_id, csum, pbloom, gossip) =
            verify_signed_pong(&pong, &nonce).expect("verify pong");
        assert_eq!(
            (node_id.as_str(), csum, pbloom.as_str()),
            ("susi-daemon-node", 0, "abcd")
        );
        assert_eq!(gossip, roster);
    }

    #[test]
    fn pong_with_wrong_nonce_or_mac_is_rejected() {
        let _t = set_key_env();
        let (ping, nonce) = signed_ping("CORE", 1, "00").unwrap();
        let (_, _, _, _, echoed) = verify_signed_ping(&ping).unwrap();
        let pong = signed_pong("peer", 0, "00", &echoed, &[]).unwrap();
        // Wrong expected nonce -> reject.
        assert!(verify_signed_pong(&pong, "deadbeef").is_none());
        // Tampered mac -> reject.
        let tampered = pong.replacen(":00:", ":11:", 1);
        assert!(verify_signed_pong(&tampered, &nonce).is_none());
        // Unsigned legacy pong never verifies.
        assert!(verify_signed_pong("SUSI_PONG:daemon:0:", &nonce).is_none());
    }

    #[test]
    fn legacy_six_field_pong_still_verifies_with_empty_roster() {
        let _t = set_key_env();
        let (ping, nonce) = signed_ping("CORE", 1, "00").unwrap();
        let (_, _, _, _, echoed) = verify_signed_ping(&ping).unwrap();
        // Reconstruct a pre-gossip pong: no roster field, MAC over the
        // original five fields — older daemons still handshake cleanly.
        let key = cluster_key().unwrap();
        let mac = mac_tag(&key, &["pong", "old-peer", "0", "00", &echoed]);
        let legacy = format!("SUSI_PONG_SIG:old-peer:0:00:{echoed}:{mac}");
        let (node_id, _, _, roster) = verify_signed_pong(&legacy, &nonce).expect("legacy pong");
        assert_eq!(node_id, "old-peer");
        assert!(roster.is_empty());
    }

    #[test]
    fn legacy_six_field_ping_verifies_with_empty_node_id() {
        let _t = set_key_env();
        // Reconstruct a pre-identity ping: no node_id field, MAC over the
        // original four fields — older daemons still handshake cleanly.
        let key = cluster_key().unwrap();
        let mac = mac_tag(&key, &["ping", "CORE", "1", "00", "noncenonce"]);
        let legacy = format!("SUSI_PING_SIG:CORE:1:00:noncenonce:{mac}");
        let (pinger_id, caps, checksum, bloom, echoed) =
            verify_signed_ping(&legacy).expect("legacy ping");
        assert!(pinger_id.is_empty());
        assert_eq!(
            (caps.as_str(), checksum, bloom.as_str(), echoed.as_str()),
            ("CORE", 1, "00", "noncenonce")
        );
    }

    #[test]
    fn node_id_persists_and_is_wire_safe() {
        let _t = set_key_env();
        let first = node_id().expect("node id");
        assert!(first.starts_with("susi-node-"));
        // Second call reads back the same identity — identity is stable.
        assert_eq!(node_id().as_deref(), Some(first.as_str()));
        // And it survives the ping wire format unchanged.
        let (ping, _) = signed_ping("CORE", 1, "00").unwrap();
        let (pinger_id, _, _, _, _) = verify_signed_ping(&ping).unwrap();
        assert_eq!(pinger_id, first);
    }

    #[test]
    fn roster_codec_roundtrips_and_caps_entries() {
        assert_eq!(encode_roster(&[]), "0");
        assert!(decode_roster("0").is_empty());
        assert!(decode_roster("zz-not-hex").is_empty());
        let many: Vec<(String, String)> = (0..20)
            .map(|i| (format!("n{i}"), format!("10.0.0.{i}:9090")))
            .collect();
        let decoded = decode_roster(&encode_roster(&many));
        assert_eq!(decoded.len(), ROSTER_GOSSIP_MAX);
        assert_eq!(decoded[0], ("n0".to_string(), "10.0.0.0:9090".to_string()));
    }
}
