//! Shared cluster key (`~/.susi/cluster.key`, 0600) and the signed LAN
//! discovery handshake behind authenticated peer membership (VC-200-001).
//!
//! Protocol (`susi-peer-v1`):
//! - Scout sends `SUSI_PING_SIG:<caps_csv>:<checksum>:<bloom_hex>:<nonce>:<mac>`
//!   where `mac` is HMAC-SHA256 over `susi-peer-v1:ping:<same fields>`.
//! - The daemon verifies the MAC and answers
//!   `SUSI_PONG_SIG:<node_id>:<checksum>:<bloom_hex>:<nonce>:<mac>` — the nonce
//!   echoed back binds the pong to this exchange, so replaying a captured
//!   signed pong against a different ping fails verification.
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
    susi_paths::SusiDirs::config_dir().join("cluster.key")
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
    let dir = susi_paths::SusiDirs::config_dir();
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

/// Build a signed discovery ping. Returns `(wire_message, nonce)` — the nonce
/// must be kept to verify the answering pong. `None` when no key exists or a
/// field is not wire-safe.
pub fn signed_ping(caps_csv: &str, checksum: u64, bloom_hex: &str) -> Option<(String, String)> {
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

/// Verified fields of a signed ping: `(caps_csv, checksum, bloom_hex, nonce)`.
/// `None` for malformed input or a bad MAC — the caller must not answer.
pub fn verify_signed_ping(msg: &str) -> Option<(String, u64, String, String)> {
    let key = cluster_key()?;
    let parts: Vec<&str> = msg.split(':').collect();
    // SUSI_PING_SIG:<caps>:<checksum>:<bloom>:<nonce>:<mac>
    if parts.len() != 6 || parts[0] != "SUSI_PING_SIG" {
        return None;
    }
    let (caps, checksum_s, bloom, nonce, mac) = (parts[1], parts[2], parts[3], parts[4], parts[5]);
    for f in [caps, checksum_s, bloom, nonce] {
        if !wire_safe(f) {
            return None;
        }
    }
    let expected = mac_tag(&key, &["ping", caps, checksum_s, bloom, nonce]);
    if expected != mac {
        return None;
    }
    Some((
        caps.to_string(),
        checksum_s.parse().unwrap_or(0),
        bloom.to_string(),
        nonce.to_string(),
    ))
}

/// Build a signed pong bound to `nonce` (echoed from the verified ping).
pub fn signed_pong(node_id: &str, checksum: u64, bloom_hex: &str, nonce: &str) -> Option<String> {
    let key = cluster_key()?;
    let checksum = checksum.to_string();
    for f in [node_id, &checksum, bloom_hex, nonce] {
        if !wire_safe(f) {
            return None;
        }
    }
    let mac = mac_tag(&key, &["pong", node_id, &checksum, bloom_hex, nonce]);
    Some(format!(
        "SUSI_PONG_SIG:{node_id}:{checksum}:{bloom_hex}:{nonce}:{mac}"
    ))
}

/// Verified fields of a signed pong: `(node_id, checksum, bloom_hex)`.
/// `None` for malformed input, a bad MAC, or a nonce that does not match the
/// nonce sent with our ping (`expected_nonce`).
pub fn verify_signed_pong(msg: &str, expected_nonce: &str) -> Option<(String, u64, String)> {
    let key = cluster_key()?;
    let parts: Vec<&str> = msg.split(':').collect();
    // SUSI_PONG_SIG:<node_id>:<checksum>:<bloom>:<nonce>:<mac>
    if parts.len() != 6 || parts[0] != "SUSI_PONG_SIG" {
        return None;
    }
    let (node_id, checksum_s, bloom, nonce, mac) =
        (parts[1], parts[2], parts[3], parts[4], parts[5]);
    for f in [node_id, checksum_s, bloom, nonce] {
        if !wire_safe(f) {
            return None;
        }
    }
    if nonce != expected_nonce {
        return None;
    }
    let expected = mac_tag(&key, &["pong", node_id, checksum_s, bloom, nonce]);
    if expected != mac {
        return None;
    }
    Some((
        node_id.to_string(),
        checksum_s.parse().unwrap_or(0),
        bloom.to_string(),
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
        let (caps, checksum, bloom, echoed) = verify_signed_ping(&ping).expect("verify ping");
        assert_eq!(
            (caps.as_str(), checksum, bloom.as_str()),
            ("CORE,GPU", 7, "00ff")
        );
        assert_eq!(echoed, nonce);
        let pong = signed_pong("susi-daemon-node", 0, "abcd", &echoed).expect("signed pong");
        let (node_id, csum, pbloom) = verify_signed_pong(&pong, &nonce).expect("verify pong");
        assert_eq!(
            (node_id.as_str(), csum, pbloom.as_str()),
            ("susi-daemon-node", 0, "abcd")
        );
    }

    #[test]
    fn pong_with_wrong_nonce_or_mac_is_rejected() {
        let _t = set_key_env();
        let (ping, nonce) = signed_ping("CORE", 1, "00").unwrap();
        let (_, _, _, echoed) = verify_signed_ping(&ping).unwrap();
        let pong = signed_pong("peer", 0, "00", &echoed).unwrap();
        // Wrong expected nonce -> reject.
        assert!(verify_signed_pong(&pong, "deadbeef").is_none());
        // Tampered mac -> reject.
        let tampered = pong.replacen(":00:", ":11:", 1);
        assert!(verify_signed_pong(&tampered, &nonce).is_none());
        // Unsigned legacy pong never verifies.
        assert!(verify_signed_pong("SUSI_PONG:daemon:0:", &nonce).is_none());
    }
}
