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
    if let Some(raw) = cached_file_bytes(&path) {
        let text = String::from_utf8(raw).ok()?;
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

fn key_bytes_from_file(path: &std::path::Path) -> Option<[u8; 32]> {
    let bytes = hex::decode(String::from_utf8(cached_file_bytes(path)?).ok()?.trim()).ok()?;
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
    if let Some(raw) = cached_file_bytes(&path) {
        let id = String::from_utf8(raw).ok()?.trim().to_string();
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

// ---- Pairwise channel encryption (bound members only) ----

/// This node's X25519 static secret, converted from `node.key` via the
/// standard birational map (libsodium `crypto_sign_ed25519_sk_to_
/// curve25519` convention: SHA-512 of the 32-byte seed, lower half —
/// clamping happens inside the Montgomery scalar multiply). Pairs
/// with `peer_x25519_public`'s `VerifyingKey::to_montgomery()`.
fn node_x25519_secret() -> Option<x25519_dalek::StaticSecret> {
    let sk = node_signing_key()?;
    let h = sha2::Sha512::digest(sk.to_bytes());
    let mut b = [0u8; 32];
    b.copy_from_slice(&h[..32]);
    Some(x25519_dalek::StaticSecret::from(b))
}

/// A peer's X25519 public key from its roster-bound Ed25519 pubkey —
/// the same birational map `node_x25519_secret` uses on our side.
fn peer_x25519_public(pubkey_hex: &str) -> Option<x25519_dalek::PublicKey> {
    let pk = hex::decode(pubkey_hex).ok()?;
    let arr = <[u8; 32]>::try_from(pk.as_slice()).ok()?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&arr).ok()?;
    Some(x25519_dalek::PublicKey::from(vk.to_montgomery().to_bytes()))
}

/// AEAD key shared by this node and the bound member `peer_pubkey_hex`:
/// X25519 ECDH run through HMAC-SHA-256 with a domain separator so the
/// channel key never coincides with another protocol's use of the same
/// shared point.
fn peer_channel_key(peer_pubkey_hex: &str) -> Option<[u8; 32]> {
    let shared = node_x25519_secret()?.diffie_hellman(&peer_x25519_public(peer_pubkey_hex)?);
    Some(hmac_sha256(shared.as_bytes(), b"susi-peer-enc-v1"))
}

/// Seal `plaintext` for the bound member `peer_pubkey_hex` →
/// `(nonce_hex, ciphertext)` under ChaCha20-Poly1305. `None` when this
/// node or the peer has no usable key material — callers degrade to
/// plaintext (unbound-member mode), never to a wrong key.
pub fn member_seal(peer_pubkey_hex: &str, plaintext: &[u8]) -> Option<(String, Vec<u8>)> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    let key = peer_channel_key(peer_pubkey_hex)?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).ok()?;
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
    let ct = cipher
        .encrypt(chacha20poly1305::Nonce::from_slice(&nonce), plaintext)
        .ok()?;
    Some((hex::encode(nonce), ct))
}

/// Open a `member_seal` ciphertext from the bound member
/// `peer_pubkey_hex`. `None` on any failure (bad key, tampered tag,
/// malformed nonce) — there is no partial plaintext.
pub fn member_open(peer_pubkey_hex: &str, nonce_hex: &str, ciphertext: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    let key = peer_channel_key(peer_pubkey_hex)?;
    let nonce = hex::decode(nonce_hex).ok()?;
    let nonce: &[u8; 12] = nonce.as_slice().try_into().ok()?;
    let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
    cipher
        .decrypt(chacha20poly1305::Nonce::from_slice(nonce), ciphertext)
        .ok()
}

/// The bound Ed25519 pubkey of the roster member registered at `addr`
/// (`host:port`); `None` when no bound member claims that address —
/// the client's signal to send plaintext instead of a seal nobody can
/// open.
/// The parsed `peers.json`/`peers_banned.json` rows, cached by file
/// (mtime_ns, len): the request-auth hot path consults the roster
/// several times per call — a stat is microseconds, a read+parse of
/// the whole roster is not. A stamp change reloads; a missing file
/// caches as empty so a deleted roster doesn't re-stat-then-parse
/// fail every request.
pub fn config_json_rows(file: &str) -> Vec<serde_json::Value> {
    use std::sync::{Mutex, OnceLock};
    type RowsCache = Mutex<std::collections::HashMap<String, (u128, u64, Vec<serde_json::Value>)>>;
    static CACHE: OnceLock<RowsCache> = OnceLock::new();
    let path = crate::susi_paths::SusiDirs::config_dir().join(file);
    let stamp = fs::metadata(&path)
        .ok()
        .and_then(|m| {
            m.modified().ok().map(|t| {
                (
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                    m.len(),
                )
            })
        })
        .unwrap_or((0, 0));
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let key = path.to_string_lossy().to_string();
    {
        let map = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((ns, len, rows)) = map.get(&key) {
            if (*ns, *len) == stamp {
                return rows.clone();
            }
        }
    }
    let rows = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .unwrap_or_default();
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (stamp.0, stamp.1, rows.clone()));
    rows
}

/// Small-credential-file cache, same mtime-keyed pattern as
/// `config_json_rows`: `cluster.key`/`node.key`/`node_id` are read on
/// every signed request, handshake verify, and seal — a stat replaces
/// the read+decode. Absent files are NOT cached (creation paths must
/// still run); a stamp change or read failure drops the entry so a
/// rekey never serves a stale key.
pub fn cached_file_bytes(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::sync::{Mutex, OnceLock};
    type BytesCache = Mutex<std::collections::HashMap<std::path::PathBuf, ((u128, u64), Vec<u8>)>>;
    static CACHE: OnceLock<BytesCache> = OnceLock::new();
    let meta = fs::metadata(path).ok()?;
    let stamp = (
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        meta.len(),
    );
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let key = path.to_path_buf();
    {
        let map = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, bytes)) = map.get(&key) {
            if *s == stamp {
                return Some(bytes.clone());
            }
        }
    }
    let bytes = fs::read(path).ok()?;
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (stamp, bytes.clone()));
    Some(bytes)
}

pub fn bound_pubkey_for_addr(addr: &str) -> Option<String> {
    let roster = config_json_rows("peers.json");
    let host = addr.split(':').next()?;
    let bound = |m: &serde_json::Value| {
        let pk = m.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        (!pk.is_empty()).then(|| pk.to_string())
    };
    // Exact host:port first — two members on one host must not cross-seal
    // — then a host-only fallback for legacy rows that stored bare IPs.
    roster
        .iter()
        .find(|m| m.get("address").and_then(|v| v.as_str()) == Some(addr))
        .and_then(bound)
        .or_else(|| {
            roster
                .iter()
                .find(|m| {
                    m.get("address")
                        .and_then(|v| v.as_str())
                        .and_then(|a| a.split(':').next())
                        == Some(host)
                })
                .and_then(bound)
        })
}

/// The bound Ed25519 pubkey of roster member `node_id` — the receiver-
/// side mirror of `bound_pubkey_for_addr`, used to open sealed bodies
/// from members whose keys the ledger/handshake already attested.
pub fn bound_pubkey_for_node(node_id: &str) -> Option<String> {
    let roster = config_json_rows("peers.json");
    roster.iter().find_map(|m| {
        if m.get("node_id").and_then(|v| v.as_str()) != Some(node_id) {
            return None;
        }
        let pk = m.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        (!pk.is_empty()).then(|| pk.to_string())
    })
}

// ---- Subject-signed binding attestation ----

/// The payload a member signs to attest its own key binding —
/// `susi-bind-v1:{node_id}:{pubkey}`. Carried in v3 pongs and on
/// `member_add` records (`subject_sig`) so the member_add's pubkey is
/// no longer merely proposer-asserted: a binding a subject never
/// signed cannot be committed.
pub fn bind_payload(node_id: &str, pubkey: &str) -> String {
    format!("susi-bind-v1:{node_id}:{pubkey}")
}

/// This node's binding attestation — Ed25519 sig over `bind_payload`
/// for our own id+pubkey. `None` without a usable `node.key`.
pub fn bind_attestation() -> Option<String> {
    let id = wire_node_id();
    let pk = node_pubkey_hex()?;
    member_sign(&bind_payload(&id, &pk))
}

/// Verify `sig` as `node_id`'s attestation for `pubkey` — strict
/// verification under the *claimed* key; a wrong-key binding produces
/// a sig that cannot verify and the record/pong is refused.
pub fn verify_bind_attestation(node_id: &str, pubkey: &str, sig: &str) -> bool {
    !pubkey.is_empty()
        && !sig.is_empty()
        && member_verify(pubkey, &bind_payload(node_id, pubkey), sig)
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

/// V3 pong (`SUSI_PONG_SIG3`) — v2 plus `bind_sig`: the responder's
/// Ed25519 attestation over `susi-bind-v1:{node_id}:{pubkey}` (see
/// `bind_attestation`). The HMAC authenticates the datagram; the
/// bind_sig proves the claimed pubkey is the responder's own choice —
/// a `member_add` built from this handshake carries it as
/// `subject_sig`, so wrong-key bindings can't be proposed at all.
pub fn signed_pong_v3(
    node_id: &str,
    checksum: u64,
    bloom_hex: &str,
    nonce: &str,
    roster: &[(String, String)],
) -> Option<String> {
    let key = cluster_key()?;
    let pubkey = node_pubkey_hex()?;
    let bind_sig = bind_attestation()?;
    let checksum = checksum.to_string();
    let roster_hex = encode_roster(&roster[..roster.len().min(32)]);
    for f in [
        node_id,
        &pubkey,
        &bind_sig,
        &checksum,
        bloom_hex,
        nonce,
        &roster_hex,
    ] {
        if !wire_safe(f) {
            return None;
        }
    }
    let mac = mac_tag(
        &key,
        &[
            "pong3",
            node_id,
            &pubkey,
            &bind_sig,
            &checksum,
            bloom_hex,
            nonce,
            &roster_hex,
        ],
    );
    Some(format!(
        "SUSI_PONG_SIG3:{node_id}:{pubkey}:{bind_sig}:{checksum}:{bloom_hex}:{nonce}:{roster_hex}:{mac}"
    ))
}

/// Verified fields of a signed pong: `(node_id, checksum, bloom_hex,
/// roster, pubkey, bind_sig)` — `pubkey` is the responder's attested
/// Ed25519 verifying key (empty for pre-v2 pongs); `bind_sig` is its
/// signature over `susi-bind-v1:{node_id}:{pubkey}` (empty for pre-v3
/// pongs) — a `member_add` subject attestation sourced from the
/// handshake rather than the proposer.
pub type VerifiedPong = (String, u64, String, Vec<(String, String)>, String, String);

/// Verify a signed pong against `expected_nonce`. `None` for
/// malformed input, a bad MAC, or a nonce that does not match the
/// nonce sent with our ping. Legacy 6-field pongs (pre-gossip
/// builds) verify with an empty roster so mixed-version clusters
/// still handshake; v1 7-field pongs verify with an empty pubkey.
pub fn verify_signed_pong(msg: &str, expected_nonce: &str) -> Option<VerifiedPong> {
    let key = cluster_key()?;
    let parts: Vec<&str> = msg.split(':').collect();
    // SUSI_PONG_SIG3:<node_id>:<pubkey>:<bind_sig>:<checksum>:<bloom>:<nonce>:<roster>:<mac>
    // SUSI_PONG_SIG2:<node_id>:<pubkey>:<checksum>:<bloom>:<nonce>:<roster>:<mac>
    // SUSI_PONG_SIG:<node_id>:<checksum>:<bloom>:<nonce>:[<roster>:]<mac>
    let (node_id, pubkey, bind_sig, checksum_s, bloom, nonce, roster_hex, mac) =
        match (parts.first(), parts.len()) {
            (Some(&"SUSI_PONG_SIG3"), 9) => (
                parts[1], parts[2], parts[3], parts[4], parts[5], parts[6], parts[7], parts[8],
            ),
            (Some(&"SUSI_PONG_SIG2"), 8) => (
                parts[1], parts[2], "", parts[3], parts[4], parts[5], parts[6], parts[7],
            ),
            (Some(&"SUSI_PONG_SIG"), 7) => (
                parts[1], "", "", parts[2], parts[3], parts[4], parts[5], parts[6],
            ),
            (Some(&"SUSI_PONG_SIG"), 6) => (
                parts[1], "", "", parts[2], parts[3], parts[4], "0", parts[5],
            ),
            _ => return None,
        };
    for f in [node_id, checksum_s, bloom, nonce, roster_hex] {
        if !wire_safe(f) {
            return None;
        }
    }
    // pubkey/bind_sig may be empty (pre-v2/v3 pongs) but never malformed
    // when present.
    for f in [pubkey, bind_sig] {
        if !f.is_empty() && !wire_safe(f) {
            return None;
        }
    }
    if nonce != expected_nonce {
        return None;
    }
    let expected = match (parts.first(), parts.len()) {
        (Some(&"SUSI_PONG_SIG3"), _) => mac_tag(
            &key,
            &[
                "pong3", node_id, pubkey, bind_sig, checksum_s, bloom, nonce, roster_hex,
            ],
        ),
        (Some(&"SUSI_PONG_SIG2"), _) => mac_tag(
            &key,
            &[
                "pong2", node_id, pubkey, checksum_s, bloom, nonce, roster_hex,
            ],
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
    // A v3 pong must carry a *verifying* binding attestation — a sig
    // that doesn't prove against the claimed pubkey means the peer is
    // advertising a key it does not hold; refuse the handshake.
    if !bind_sig.is_empty() && !verify_bind_attestation(node_id, pubkey, bind_sig) {
        return None;
    }
    Some((
        node_id.to_string(),
        checksum_s.parse().unwrap_or(0),
        bloom.to_string(),
        decode_roster(roster_hex),
        pubkey.to_string(),
        bind_sig.to_string(),
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
        let (pinger_id, caps, checksum, bloom, echoed, wants_v2) =
            verify_signed_ping(&ping).expect("verify ping");
        assert!(!wants_v2);
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
        let (node_id, csum, pbloom, gossip, pubkey, bind_sig) =
            verify_signed_pong(&pong, &nonce).expect("verify pong");
        assert!(pubkey.is_empty());
        assert!(bind_sig.is_empty());
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
        let (_, _, _, _, echoed, _) = verify_signed_ping(&ping).unwrap();
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
        let (_, _, _, _, echoed, _) = verify_signed_ping(&ping).unwrap();
        // Reconstruct a pre-gossip pong: no roster field, MAC over the
        // original five fields — older daemons still handshake cleanly.
        let key = cluster_key().unwrap();
        let mac = mac_tag(&key, &["pong", "old-peer", "0", "00", &echoed]);
        let legacy = format!("SUSI_PONG_SIG:old-peer:0:00:{echoed}:{mac}");
        let (node_id, _, _, roster, _, _) =
            verify_signed_pong(&legacy, &nonce).expect("legacy pong");
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
        let (pinger_id, caps, checksum, bloom, echoed, _) =
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
        let (pinger_id, _, _, _, _, _) = verify_signed_ping(&ping).unwrap();
        assert_eq!(pinger_id, first);
    }

    #[test]
    fn peer_channel_ecdh_is_symmetric_and_seal_roundtrips() {
        let _t = set_key_env();
        let our_pub = node_pubkey_hex().expect("node pubkey");
        // The conversion invariant that makes seal/open agree across
        // two different nodes: the X25519 public derived from our
        // secret must equal the Montgomery map of our Ed25519 pubkey.
        let sk = node_x25519_secret().expect("x25519 secret");
        assert_eq!(
            x25519_dalek::PublicKey::from(&sk).as_bytes(),
            peer_x25519_public(&our_pub)
                .expect("peer x25519")
                .as_bytes()
        );
        let (nonce, ct) = member_seal(&our_pub, b"hello peer").expect("seal");
        assert_eq!(
            member_open(&our_pub, &nonce, &ct).expect("open"),
            b"hello peer"
        );
        // Tampered ciphertext and a wrong nonce both fail closed.
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert!(member_open(&our_pub, &nonce, &bad).is_none());
        assert!(member_open(&our_pub, &"00".repeat(12), &ct).is_none());
    }

    #[test]
    fn v3_pong_carries_subject_attested_binding() {
        let _t = set_key_env();
        let (ping, nonce) = signed_ping_v2("CORE", 1, "00").expect("v2 ping");
        let (_, _, _, _, echoed, wants_v2) = verify_signed_ping(&ping).expect("verify");
        assert!(wants_v2);
        let my_id = wire_node_id();
        let my_pk = node_pubkey_hex().expect("pubkey");
        let pong = signed_pong_v3(&my_id, 3, "abcd", &echoed, &[]).expect("v3 pong");
        let (node_id, csum, _bloom, _roster, pubkey, bind_sig) =
            verify_signed_pong(&pong, &nonce).expect("v3 verify");
        assert_eq!(node_id, my_id);
        assert_eq!(csum, 3);
        assert_eq!(pubkey, my_pk);
        // The attestation must verify under the claimed key for the
        // claimed id — this is what member_add's subject_sig carries.
        assert!(verify_bind_attestation(&node_id, &pubkey, &bind_sig));
        // Wrong id, wrong key, and tampered sig all fail closed.
        assert!(!verify_bind_attestation(
            "susi-node-other",
            &pubkey,
            &bind_sig
        ));
        assert!(!verify_bind_attestation(
            &node_id,
            &"aa".repeat(32),
            &bind_sig
        ));
        let mut bad = bind_sig.clone();
        bad.replace_range(0..2, "00");
        assert!(!verify_bind_attestation(&node_id, &pubkey, &bad));
        // The real attack: a member holding cluster.key mints a
        // correctly-MAC'd v3 pong claiming a key the subject never
        // attested — the binding still refuses because the sig can't
        // verify under the claimed pubkey.
        let key = cluster_key().expect("cluster key");
        let fake_sig = "00".repeat(64);
        let mac = mac_tag(
            &key,
            &[
                "pong3", &my_id, &my_pk, &fake_sig, "3", "abcd", &echoed, "0",
            ],
        );
        let forged = format!("SUSI_PONG_SIG3:{my_id}:{my_pk}:{fake_sig}:3:abcd:{echoed}:0:{mac}");
        assert!(verify_signed_pong(&forged, &nonce).is_none());
        // And a v2 pong still verifies — with an empty attestation, so
        // member_add can't manufacture subject_sig from it.
        let pong2 = signed_pong_v2(&my_id, 3, "abcd", &echoed, &[]).expect("v2 pong");
        let (_, _, _, _, _, sig2) = verify_signed_pong(&pong2, &nonce).expect("v2 verify");
        assert!(sig2.is_empty());
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
