//! Shared cluster key (`~/.susi/cluster.key`, 0600) and the signed LAN
//! discovery handshake behind authenticated peer membership (VC-200-001).
//!
//! Protocol (`susi-peer-v1`):
//! - Scout sends the bound ping
//!   `SUSI_PING_SIG3:<node_id>:<caps_csv>:<checksum>:<bloom_hex>:<service_hex>:<ts>:<nonce>:<mac>`
//!   where `node_id` is the sender's persisted identity (`node_id()`),
//!   `service_hex` hex-encodes the sender's source IP toward this receiver
//!   plus its HTTP alias port, `ts` is Unix seconds and `mac` is HMAC-SHA256
//!   over `susi-peer-v1:ping3:<same fields>`. The receiver requires the
//!   datagram's source IP to equal the advertised one, the timestamp to be
//!   within `PING_WINDOW_SECS`, and the nonce to be unseen
//!   (`verify_bound_ping`), so a captured ping neither admits another
//!   address nor replays. The unbound `SUSI_PING_SIG` / `SUSI_PING_SIG2`
//!   dialects are retired: they MAC nothing about the sender, and a daemon
//!   neither answers nor admits them.
//! - The daemon verifies the ping and answers
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
use std::path::{Path, PathBuf};

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
    let raw = match cached_file_bytes(&path) {
        Some(raw) if !is_torn_identity(&raw) => raw,
        // Absent, or an empty file left by an interrupted creation (never
        // a usable key, so replacing it cuts no one off the cluster).
        _ => {
            let mut fresh = [0u8; 32];
            getrandom::fill(&mut fresh).ok()?;
            install_identity_file(&path, susi_paths::hex::encode(fresh).as_bytes())?
        }
    };
    let bytes = std::str::from_utf8(&raw)
        .ok()
        .and_then(|text| susi_paths::hex::decode(text.trim()).ok());
    if let Some(bytes) = bytes.filter(|b| b.len() == 32) {
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
    None
}

/// An identity file holding nothing but whitespace is a creation that was
/// interrupted between create and write, not a real key or id.
fn is_torn_identity(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_whitespace)
}

/// Install `fresh` as an owner-only identity file unless one already exists,
/// and return the bytes on disk afterwards (the winner's, if another process
/// raced us). Creation goes through `install_private_file` — staged in full,
/// then hard-linked into place — because create-then-write left an empty
/// file behind on a crash, which `cluster_key` refused to rotate and
/// `node_id` could never replace. Such a torn file is cleared under a lock
/// so only one process recovers it.
fn install_identity_file(path: &Path, fresh: &[u8]) -> Option<Vec<u8>> {
    let torn_on_disk = |p: &Path| fs::read(p).is_ok_and(|b| is_torn_identity(&b));
    let _recovery_lock = if torn_on_disk(path) {
        let dir = path.parent()?;
        let name = path.file_name()?.to_str()?;
        let lock = super::file_lock::FileLock::acquire(dir, name)?;
        if torn_on_disk(path) {
            fs::remove_file(path).ok()?;
        }
        Some(lock)
    } else {
        None
    };
    super::install_private_file(path, fresh).ok()?;
    fs::read(path).ok()
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
    susi_paths::hex::encode(hmac_sha256(key, message))
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
    susi_paths::SusiDirs::config_dir().join("cluster.key.next")
}

/// Prior-epoch key retained on activation (`prev_key_at`). Verification
/// accepts it only for chain-pinned history fills — see
/// `commit_log::append_to`'s epoch rule — so a revoked key can never
/// authorize new records. It stays verifiable for audit only within
/// its bounded overlap window (see [`PREV_KEY_OVERLAP_SECS`]), or
/// until [`revoke_prev_key`] drops it early.
///
/// How long a staged next-epoch key may wait before activation is
/// refused. A credential staged longer ago than this cannot silently
/// activate on fingerprint match alone — the coordinator must re-stage
/// with a fresh fingerprint, closing the "stale staged key activates
/// regardless of age" gap.
const MAX_STAGE_AGE_SECS: u64 = 24 * 60 * 60;

/// How long a retired key remains usable for verifying pre-rotation
/// history after a rotation — bounded overlap, not indefinite
/// retention of old key material.
const PREV_KEY_OVERLAP_SECS: u64 = 7 * 24 * 60 * 60;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Companion path for a timestamp sidecar file next to `path`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn write_timestamp_file(path: &Path, unix_secs: u64) -> bool {
    super::json_util::atomic_write_bytes(path, unix_secs.to_string().as_bytes()).is_ok()
}

fn read_timestamp_file(path: &Path) -> Option<u64> {
    String::from_utf8(cached_file_bytes(path)?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn key_bytes_from_file(path: &std::path::Path) -> Option<[u8; 32]> {
    let bytes =
        susi_paths::hex::decode(String::from_utf8(cached_file_bytes(path)?).ok()?.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(key)
}

/// Write a key file with 0600 mode atomically (unique staging + rename).
fn write_key_file(path: &Path, key: &[u8; 32]) -> bool {
    super::json_util::atomic_write_bytes(path, susi_paths::hex::encode(key).as_bytes()).is_ok()
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
    susi_paths::hex::encode(Sha256::digest(key))
}

/// Stage a next-epoch key beside `cluster.key` (0600). The staged
/// file is inert until a committed `cluster_rekey` record activates
/// it — writing this file alone changes nothing.
pub fn stage_key(key: &[u8; 32]) -> bool {
    stage_key_to(key, &staged_key_path())
}

/// Test seam: stage to an explicit path. Also records when the key was
/// staged, so a stale staged key cannot activate indefinitely — see
/// [`MAX_STAGE_AGE_SECS`].
pub fn stage_key_to(key: &[u8; 32], path: &Path) -> bool {
    write_key_file(path, key) && write_timestamp_file(&sibling(path, ".staged_at"), now_unix())
}

/// Load a staged next-epoch key, if one is waiting for activation.
pub fn staged_key() -> Option<[u8; 32]> {
    key_bytes_from_file(&staged_key_path())
}

/// The prior-epoch key, retained after activation for verifying
/// pre-rotation history — but only within its bounded overlap window.
/// `None` when no rotation has happened, the window has elapsed, or
/// the key was explicitly revoked.
pub fn prev_key() -> Option<[u8; 32]> {
    prev_key_at(&susi_paths::SusiDirs::config_dir(), now_unix())
}

/// Test seam: as [`prev_key`], against an explicit directory and clock.
pub fn prev_key_at(dir: &Path, now_unix_secs: u64) -> Option<[u8; 32]> {
    let prev_path = dir.join("cluster.key.prev");
    let key = key_bytes_from_file(&prev_path)?;
    let expires_at = read_timestamp_file(&sibling(&prev_path, ".expires_at"))?;
    if now_unix_secs >= expires_at {
        return None;
    }
    Some(key)
}

/// Explicitly revoke the retired key before its overlap window elapses
/// — e.g. a suspected-compromised member. After this, [`prev_key`] can
/// no longer verify pre-rotation history through this path, regardless
/// of how much of the overlap window remained.
pub fn revoke_prev_key() -> bool {
    revoke_prev_key_at(&susi_paths::SusiDirs::config_dir())
}

/// Test seam: as [`revoke_prev_key`], against an explicit directory.
pub fn revoke_prev_key_at(dir: &Path) -> bool {
    let prev_path = dir.join("cluster.key.prev");
    let removed = fs::remove_file(&prev_path).is_ok();
    let _ = fs::remove_file(sibling(&prev_path, ".expires_at"));
    removed
}

/// Activate the staged key iff its fingerprint matches `fingerprint`
/// — the committed rekey record's value. On success the current key
/// rotates to `cluster.key.prev` (with a fresh bounded-overlap expiry)
/// and the staged key becomes current. Returns false when no staged
/// key exists, the fingerprint does not match, or the staged key has
/// waited past [`MAX_STAGE_AGE_SECS`]: applying a rekey record can
/// never clear the key or activate an unexpected or stale one.
pub fn activate_staged_key(fingerprint: &str) -> bool {
    activate_staged_key_at(fingerprint, &susi_paths::SusiDirs::config_dir())
}

/// Test seam: activate against an explicit directory.
pub fn activate_staged_key_at(fingerprint: &str, dir: &std::path::Path) -> bool {
    let staged_path = dir.join("cluster.key.next");
    let staged_at_path = sibling(&staged_path, ".staged_at");
    let Some(staged) = key_bytes_from_file(&staged_path) else {
        // Nothing staged, or a torn/corrupt staged file. Self-heal the
        // latter so a future legitimate stage is never blocked by
        // leftover garbage from an interrupted rotation.
        if staged_path.exists() {
            let _ = fs::remove_file(&staged_path);
            let _ = fs::remove_file(&staged_at_path);
        }
        return false;
    };
    if key_fingerprint(&staged) != fingerprint {
        return false;
    }
    let staged_at = read_timestamp_file(&staged_at_path).unwrap_or(0);
    if now_unix().saturating_sub(staged_at) > MAX_STAGE_AGE_SECS {
        // Stale: too old to activate on fingerprint match alone. Clear
        // it rather than leave it wedged — a fresh stage must re-run.
        let _ = fs::remove_file(&staged_path);
        let _ = fs::remove_file(&staged_at_path);
        return false;
    }
    let current_path = dir.join("cluster.key");
    let prev_path = dir.join("cluster.key.prev");
    if let Some(current) = key_bytes_from_file(&current_path) {
        if !write_key_file(&prev_path, &current)
            || !write_timestamp_file(
                &sibling(&prev_path, ".expires_at"),
                now_unix() + PREV_KEY_OVERLAP_SECS,
            )
        {
            return false;
        }
    }
    // rename, not copy — the staged key must not linger beside the
    // active one after rotation.
    let rotated = fs::rename(&staged_path, &current_path).is_ok();
    if rotated {
        let _ = fs::remove_file(&staged_at_path);
    }
    rotated
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
        return susi_paths::hex::encode(&digest[..16]);
    }
    susi_paths::hex::encode(raw)
}

pub(crate) fn mac_tag(key: &[u8; 32], fields: &[&str]) -> String {
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
    let path = susi_paths::SusiDirs::config_dir().join("node_id");
    let raw = match cached_file_bytes(&path) {
        Some(raw) if !is_torn_identity(&raw) => raw,
        _ => install_identity_file(
            &path,
            format!("susi-node-{}", &random_nonce_hex()[..12]).as_bytes(),
        )?,
    };
    let id = String::from_utf8(raw).ok()?.trim().to_string();
    (wire_safe(&id) && id.len() <= 64).then_some(id)
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
    susi_paths::SusiDirs::config_dir().join("node.key")
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
    node_signing_key().map(|k| susi_paths::hex::encode(k.verifying_key().to_bytes()))
}

/// Sign `payload` with this node's key → hex Ed25519 signature.
/// Commit records pass their HMAC `signature` as the payload, so the
/// member signature transitively binds every field the HMAC covers.
pub fn member_sign(payload: &str) -> Option<String> {
    let key = node_signing_key()?;
    let msg = format!("susi-member-v1:{payload}");
    Some(susi_paths::hex::encode(
        ed25519_dalek::Signer::sign(&key, msg.as_bytes()).to_bytes(),
    ))
}

/// Verify `sig_hex` under `pubkey_hex` for `payload` — strict
/// verification (rejects non-canonical encodings and small-order
/// keys). Any parse failure is a refusal, never a pass.
pub fn member_verify(pubkey_hex: &str, payload: &str, sig_hex: &str) -> bool {
    let Ok(pk) = susi_paths::hex::decode(pubkey_hex) else {
        return false;
    };
    let Ok(sig) = susi_paths::hex::decode(sig_hex) else {
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
pub(crate) fn node_x25519_secret() -> Option<x25519_dalek::StaticSecret> {
    let sk = node_signing_key()?;
    let h = sha2::Sha512::digest(sk.to_bytes());
    let mut b = [0u8; 32];
    b.copy_from_slice(&h[..32]);
    Some(x25519_dalek::StaticSecret::from(b))
}

/// A peer's X25519 public key from its roster-bound Ed25519 pubkey —
/// the same birational map `node_x25519_secret` uses on our side.
pub(crate) fn peer_x25519_public(pubkey_hex: &str) -> Option<x25519_dalek::PublicKey> {
    let pk = susi_paths::hex::decode(pubkey_hex).ok()?;
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
    Some((susi_paths::hex::encode(nonce), ct))
}

/// Open a `member_seal` ciphertext from the bound member
/// `peer_pubkey_hex`. `None` on any failure (bad key, tampered tag,
/// malformed nonce) — there is no partial plaintext.
pub fn member_open(peer_pubkey_hex: &str, nonce_hex: &str, ciphertext: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    let key = peer_channel_key(peer_pubkey_hex)?;
    let nonce = susi_paths::hex::decode(nonce_hex).ok()?;
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
    config_json_rows_at(&susi_paths::SusiDirs::config_dir(), file)
}

/// Dir-explicit form of `config_json_rows` — intake checkers take a
/// `dir` seam (tests point at temp homes), and the cache keys by full
/// path so a test dir and the live config dir never share rows.
pub fn config_json_rows_at(dir: &std::path::Path, file: &str) -> Vec<serde_json::Value> {
    config_json_rows_checked_at(dir, file).unwrap_or_default()
}

/// `config_json_rows_at` that tells "absent" (`Ok(vec![])`) apart from
/// "present but unreadable or not a JSON array" (`Err`). Guards that must
/// fail closed — the ban list above all — use this form; a damaged file
/// read as empty would readmit every evicted member.
pub fn config_json_rows_checked_at(
    dir: &std::path::Path,
    file: &str,
) -> Result<Vec<serde_json::Value>, String> {
    use std::sync::{Mutex, OnceLock};
    type Rows = Result<Vec<serde_json::Value>, String>;
    type RowsCache = Mutex<std::collections::HashMap<String, (u128, u64, Rows)>>;
    static CACHE: OnceLock<RowsCache> = OnceLock::new();
    let path = dir.join(file);
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
    let rows = read_json_rows_strict(&path);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (stamp.0, stamp.1, rows.clone()));
    rows
}

/// Uncached rows of a roster JSON file: `Ok(vec![])` when absent, `Err`
/// when present but unreadable or not a JSON array. Read-modify-write
/// paths use this so a damaged file is never rewritten from an empty view,
/// which erased every row it held.
pub fn read_json_rows_strict(path: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Vec<serde_json::Value>>(&text)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Whether any row of `peers_banned.json` under `dir` satisfies `matches`.
/// Fails closed: a ban list that exists but cannot be read or parsed
/// counts as a match, so a damaged file never readmits an evicted member.
pub fn member_banned_at(
    dir: &std::path::Path,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> bool {
    match config_json_rows_checked_at(dir, "peers_banned.json") {
        Ok(rows) => rows.iter().any(matches),
        Err(e) => {
            eprintln!(
                "[peers] ban list unreadable ({e}); refusing membership until it is repaired"
            );
            true
        }
    }
}

/// `member_banned_at` for the live config dir.
pub fn member_banned(matches: impl Fn(&serde_json::Value) -> bool) -> bool {
    member_banned_at(&susi_paths::SusiDirs::config_dir(), matches)
}

/// The rule every admission path shares: a ban row blocks a member when
/// either its node id or its address matches.
pub fn ban_row_matches(row: &serde_json::Value, node_id: &str, address: &str) -> bool {
    row.get("node_id").and_then(|v| v.as_str()) == Some(node_id)
        || row.get("address").and_then(|v| v.as_str()) == Some(address)
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

/// Wire tag of the bound discovery ping, the only ping a daemon answers.
pub const PING_TAG: &str = "SUSI_PING_SIG3";

/// Clock skew tolerated between a bound ping's timestamp and local time.
pub const PING_WINDOW_SECS: u64 = 60;

/// Nonces a receiver remembers at once. A ping is recorded only after its
/// MAC, source and timestamp verify, so only a holder of `cluster.key` can
/// fill the cache; when it is full of live entries new pings are refused
/// (fail closed) rather than forgetting a nonce that could then replay.
pub const PING_NONCE_CAPACITY: usize = 4096;

/// A verified `SUSI_PING_SIG3` ping. `service` is the sender's advertised
/// MCP endpoint: the datagram's own source IP and the sender's HTTP alias
/// port, both covered by the MAC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPing {
    pub node_id: String,
    pub caps_csv: String,
    pub checksum: u64,
    pub bloom_hex: String,
    pub service: std::net::SocketAddr,
    pub ts_secs: u64,
    pub nonce: String,
}

/// Why a discovery datagram earned no admission and no pong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingRejection {
    /// No usable `cluster.key` on this node.
    NoKey,
    /// `SUSI_PING_SIG` / `SUSI_PING_SIG2`: MACs nothing about the sender, so
    /// a captured copy replays from any address. Retired, never answered.
    Unbound,
    /// Not a ping, wrong field count, or a field outside the wire alphabet.
    Malformed,
    /// The MAC does not verify under this cluster's key.
    BadMac,
    /// The advertised address is not the datagram's source address.
    SourceMismatch,
    /// The timestamp is outside `PING_WINDOW_SECS` of local time.
    Stale,
    /// This nonce was already admitted inside the window.
    Replayed,
    /// The nonce cache is full of live entries.
    CacheFull,
}

/// Bounded, TTL'd record of admitted ping nonces, the same replay defence
/// `net_guard` keeps for signed requests. Entries expire after twice the
/// window, by which time the timestamp check refuses the ping anyway.
#[derive(Debug)]
pub struct PingNonceCache {
    seen: std::collections::HashMap<String, u64>,
    capacity: usize,
}

impl PingNonceCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: std::collections::HashMap::new(),
            capacity,
        }
    }

    /// Record `nonce` admitted at `now`; `Err` when it is already live or
    /// the cache cannot hold it.
    pub fn admit(&mut self, nonce: &str, now: u64) -> Result<(), PingRejection> {
        let ttl = 2 * PING_WINDOW_SECS;
        if self
            .seen
            .get(nonce)
            .is_some_and(|at| now.saturating_sub(*at) <= ttl)
        {
            return Err(PingRejection::Replayed);
        }
        if self.seen.len() >= self.capacity {
            self.seen.retain(|_, at| now.saturating_sub(*at) <= ttl);
        }
        if self.seen.len() >= self.capacity {
            return Err(PingRejection::CacheFull);
        }
        self.seen.insert(nonce.to_string(), now);
        Ok(())
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The local address the OS routes `dest` from: a throwaway UDP socket is
/// connected (which sends nothing) and its local address read back. Works
/// for unicast, loopback and the limited broadcast address.
pub fn route_source_ip(dest: std::net::SocketAddr) -> Option<std::net::IpAddr> {
    let any: std::net::SocketAddr = if dest.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let probe = std::net::UdpSocket::bind(any).ok()?;
    let _ = probe.set_broadcast(true);
    probe.connect(dest).ok()?;
    let ip = probe.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// Build the bound discovery ping this node sends toward `dest`. The MAC
/// covers the sender's source IP for that route plus its HTTP alias port,
/// a timestamp and a fresh nonce, so a captured copy neither verifies from
/// another address nor replays after the window or twice inside it.
/// Returns `(wire_message, nonce)`; `None` without a key, a route to
/// `dest`, or wire-safe fields.
pub fn signed_ping_v3(
    caps_csv: &str,
    checksum: u64,
    bloom_hex: &str,
    dest: std::net::SocketAddr,
) -> Option<(String, String)> {
    let key = cluster_key()?;
    let http = crate::SusiConfig::load_global()
        .map(|c| c.gmcp_http_port())
        .unwrap_or(susi_paths::ports::GMCP_HTTP);
    let service = std::net::SocketAddr::new(route_source_ip(dest)?, http);
    let nonce = random_nonce_hex();
    let frame = bound_ping_frame(
        &key,
        &wire_node_id(),
        caps_csv,
        checksum,
        bloom_hex,
        service,
        unix_now(),
        &nonce,
    )?;
    Some((frame, nonce))
}

/// The `SUSI_PING_SIG3` wire frame:
/// `SUSI_PING_SIG3:<node_id>:<caps>:<checksum>:<bloom>:<service_hex>:<ts>:<nonce>:<mac>`
/// with the MAC over `ping3` and every field in order. `service_hex` hex-
/// encodes the socket address so an IPv6 address cannot collide with the
/// `:` separator.
#[allow(clippy::too_many_arguments)] // one flat wire record: a struct would
                                     // only rename the fields it serializes
pub fn bound_ping_frame(
    key: &[u8; 32],
    node_id: &str,
    caps_csv: &str,
    checksum: u64,
    bloom_hex: &str,
    service: std::net::SocketAddr,
    ts_secs: u64,
    nonce: &str,
) -> Option<String> {
    let checksum = checksum.to_string();
    let service_hex = susi_paths::hex::encode(service.to_string());
    let ts = ts_secs.to_string();
    for f in [node_id, caps_csv, &checksum, bloom_hex, nonce] {
        if !wire_safe(f) {
            return None;
        }
    }
    let fields = [
        node_id,
        caps_csv,
        &checksum,
        bloom_hex,
        &service_hex,
        &ts,
        nonce,
    ];
    let mut macd = vec!["ping3"];
    macd.extend_from_slice(&fields);
    let mac = mac_tag(key, &macd);
    Some(format!("{PING_TAG}:{}:{mac}", fields.join(":")))
}

/// Verify a discovery datagram received from `src` against this node's
/// cluster key, the clock and the process-wide nonce cache. Only an `Ok`
/// ping may be answered or admitted.
pub fn verify_bound_ping(msg: &str, src: std::net::SocketAddr) -> Result<BoundPing, PingRejection> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<PingNonceCache>> = std::sync::OnceLock::new();
    let key = cluster_key().ok_or(PingRejection::NoKey)?;
    let cache =
        SEEN.get_or_init(|| std::sync::Mutex::new(PingNonceCache::new(PING_NONCE_CAPACITY)));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    verify_bound_ping_with(&key, msg, src, unix_now(), &mut cache)
}

/// [`verify_bound_ping`] with every input explicit. The nonce is recorded
/// only after the MAC, the source and the timestamp all verify, so forged
/// or misdirected datagrams burn no cache slot.
pub fn verify_bound_ping_with(
    key: &[u8; 32],
    msg: &str,
    src: std::net::SocketAddr,
    now: u64,
    cache: &mut PingNonceCache,
) -> Result<BoundPing, PingRejection> {
    let parts: Vec<&str> = msg.trim_end().split(':').collect();
    match parts.first() {
        Some(&PING_TAG) => {}
        Some(&"SUSI_PING_SIG" | &"SUSI_PING_SIG2") => return Err(PingRejection::Unbound),
        _ => return Err(PingRejection::Malformed),
    }
    let [_, node_id, caps, checksum_s, bloom, service_hex, ts_s, nonce, mac] = parts[..] else {
        return Err(PingRejection::Malformed);
    };
    for f in [node_id, caps, checksum_s, bloom, service_hex, ts_s, nonce] {
        if !wire_safe(f) {
            return Err(PingRejection::Malformed);
        }
    }
    let expected = mac_tag(
        key,
        &[
            "ping3",
            node_id,
            caps,
            checksum_s,
            bloom,
            service_hex,
            ts_s,
            nonce,
        ],
    );
    if !constant_time_eq(expected.as_bytes(), mac.as_bytes()) {
        return Err(PingRejection::BadMac);
    }
    let (Ok(checksum), Ok(ts_secs)) = (checksum_s.parse::<u64>(), ts_s.parse::<u64>()) else {
        return Err(PingRejection::Malformed);
    };
    let service = susi_paths::hex::decode(service_hex)
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .and_then(|text| text.parse::<std::net::SocketAddr>().ok())
        .ok_or(PingRejection::Malformed)?;
    if service.ip().to_canonical() != src.ip().to_canonical() {
        return Err(PingRejection::SourceMismatch);
    }
    if now.abs_diff(ts_secs) > PING_WINDOW_SECS {
        return Err(PingRejection::Stale);
    }
    cache.admit(nonce, now)?;
    Ok(BoundPing {
        node_id: node_id.to_string(),
        caps_csv: caps.to_string(),
        checksum,
        bloom_hex: bloom.to_string(),
        service,
        ts_secs,
        nonce: nonce.to_string(),
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    susi_paths::hex::encode(joined.as_bytes())
}

/// Inverse of `encode_roster`; `0` or undecodable input yields an
/// empty list — a malformed roster never fails the handshake itself
/// (the MAC still authenticates the field).
pub fn decode_roster(field: &str) -> Vec<(String, String)> {
    if field == "0" {
        return Vec::new();
    }
    let Ok(bytes) = susi_paths::hex::decode(field) else {
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

/// V2 pong (`SUSI_PONG_SIG2`) — answers a bound ping with this
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

/// Inline tests for the logic the external `tests/cluster_key.rs` suite does
/// not reach: pure HMAC/MAC derivation, the key-rotation lifecycle against an
/// explicit directory, strict roster/ban-list parsing, member and binding
/// signature verification, and handshake-frame rejection. Everything except
/// `frames` is independent of process-global environment.
#[cfg(test)]
mod inline_tests {
    use super::*;
    use ed25519_dalek::Signer as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KEY: [u8; 32] = {
        let mut key = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            key[i] = i as u8;
            i += 1;
        }
        key
    };
    const K1: [u8; 32] = [1; 32];
    const K2: [u8; 32] = [2; 32];

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi-ck-inline-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Cached key reads are keyed by (mtime, length): two same-length key
    /// writes to one path inside the filesystem's timestamp tick would alias.
    fn tick() {
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    #[test]
    fn hmac_matches_independent_reference_vectors() {
        // Vectors produced by Python's hmac/hashlib, not by this code.
        assert_eq!(
            hmac_sha256_hex(&KEY, b""),
            "d38b42096d80f45f826b44a9d5607de72496a415d3f4a1a8c88e3bb9da8dc1cb"
        );
        assert_eq!(
            hmac_sha256_hex(&KEY, b"susi-peer-bearer-v1"),
            "c53369e2c8891683bd3ead39cbc820bac353fb9ab9e3afaa2bd63d2fbeb1cc6c"
        );
        assert_eq!(
            hmac_sha256_hex(&[0u8; 32], b""),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad"
        );
        assert_eq!(
            susi_paths::hex::encode(hmac_sha256(&KEY, b"")),
            hmac_sha256_hex(&KEY, b"")
        );
        assert_ne!(hmac_sha256(&KEY, b"a"), hmac_sha256(&K1, b"a"));
    }

    #[test]
    fn mac_tag_is_domain_separated_and_order_sensitive() {
        assert_eq!(
            mac_tag(&KEY, &[]),
            "9d2a6a5ebb95cf09d548bb370516fc5f07ffd7292e997782226a1467dd28716f"
        );
        let ping = mac_tag(&KEY, &["ping", "n1", "caps"]);
        assert_eq!(
            ping,
            "33dfa7906d4108210382c800f199dd71b0b0ed12b38c8af75fbb609d95684c6c"
        );
        let swapped = mac_tag(&KEY, &["caps", "n1", "ping"]);
        assert_eq!(
            swapped,
            "f5ee4efd4b0a1e659f0fb2d367195e2f098deac94b609d317e041aa25fcac3a7"
        );
        assert_ne!(ping, swapped);
        assert_ne!(ping, mac_tag(&K1, &["ping", "n1", "caps"]));
        // Fields are ':'-joined, so a field containing ':' is ambiguous with
        // two fields. That is why `wire_safe` forbids ':' on every field.
        assert_eq!(mac_tag(&KEY, &["a:b"]), mac_tag(&KEY, &["a", "b"]));
        assert!(!wire_safe("a:b"));
    }

    #[test]
    fn wire_safe_admits_only_separator_free_printable_fields() {
        for ok in ["abc", "A1", "a,b", "a-b", "a_b", "a.b", "0", "nodé-1"] {
            assert!(wire_safe(ok), "{ok:?}");
        }
        for bad in [
            "", " ", "a b", "a:b", "a\nb", "a/b", "a;b", "a@b", "a\tb", "a=b",
        ] {
            assert!(!wire_safe(bad), "{bad:?}");
        }
    }

    #[test]
    fn fingerprint_nonce_and_generated_keys_have_the_documented_shape() {
        assert_eq!(
            key_fingerprint(&KEY),
            "630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd"
        );
        assert_ne!(key_fingerprint(&K1), key_fingerprint(&K2));

        let (a, b) = (generate_key().unwrap(), generate_key().unwrap());
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b, "two generated keys must differ");

        let (n1, n2) = (random_nonce_hex(), random_nonce_hex());
        assert_eq!(n1.len(), 32);
        assert!(n1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(n1, n2);
        assert!(wire_safe(&n1), "a nonce must be usable as a wire field");
    }

    #[test]
    fn identity_helpers_classify_torn_files_and_name_sidecars() {
        assert!(is_torn_identity(b""));
        assert!(is_torn_identity(b" \n\t\r"));
        assert!(!is_torn_identity(b" x "));
        assert_eq!(
            sibling(Path::new("/d/cluster.key.next"), ".staged_at"),
            PathBuf::from("/d/cluster.key.next.staged_at")
        );
    }

    #[test]
    fn key_files_round_trip_and_reject_anything_but_32_hex_bytes() {
        let dir = scratch("keyfile");
        let good = dir.join("good.key");
        assert!(write_key_file(&good, &K1));
        assert_eq!(
            fs::read_to_string(&good).unwrap(),
            susi_paths::hex::encode(K1)
        );
        assert_eq!(key_bytes_from_file(&good), Some(K1));

        let padded = dir.join("padded.key");
        fs::write(
            &padded,
            format!("  {}\n", susi_paths::hex::encode(K2).to_uppercase()),
        )
        .unwrap();
        assert_eq!(
            key_bytes_from_file(&padded),
            Some(K2),
            "case and whitespace"
        );

        for (name, content) in [
            ("short", "ab".repeat(31)),
            ("long", "ab".repeat(33)),
            ("nonhex", "zz".repeat(32)),
            ("empty", String::new()),
        ] {
            let path = dir.join(name);
            fs::write(&path, content).unwrap();
            assert_eq!(key_bytes_from_file(&path), None, "{name}");
        }
        assert_eq!(key_bytes_from_file(&dir.join("absent")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamp_files_parse_numbers_and_ignore_junk() {
        let dir = scratch("timestamp");
        let path = dir.join("t");
        assert!(write_timestamp_file(&path, 1_234_567));
        assert_eq!(read_timestamp_file(&path), Some(1_234_567));
        fs::write(&path, "  42\n").unwrap();
        assert_eq!(read_timestamp_file(&path), Some(42));
        fs::write(&path, "soon").unwrap();
        assert_eq!(read_timestamp_file(&path), None);
        assert_eq!(read_timestamp_file(&dir.join("absent")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    fn seeded(tag: &str, current: Option<[u8; 32]>) -> PathBuf {
        let dir = scratch(tag);
        if let Some(key) = current {
            assert!(write_key_file(&dir.join("cluster.key"), &key));
            tick();
        }
        dir
    }

    fn stage(dir: &Path, key: &[u8; 32]) {
        assert!(stage_key_to(key, &dir.join("cluster.key.next")));
    }

    #[test]
    fn activating_the_staged_key_rotates_current_to_a_bounded_prev() {
        let dir = seeded("rotate", Some(K1));
        stage(&dir, &K2);
        assert!(activate_staged_key_at(&key_fingerprint(&K2), &dir));

        assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K2));
        assert!(!dir.join("cluster.key.next").exists());
        assert!(!dir.join("cluster.key.next.staged_at").exists());

        let now = now_unix();
        assert_eq!(prev_key_at(&dir, now), Some(K1));
        assert_eq!(
            prev_key_at(&dir, now + PREV_KEY_OVERLAP_SECS - 100),
            Some(K1),
            "still inside the overlap window"
        );
        assert_eq!(
            prev_key_at(&dir, now + PREV_KEY_OVERLAP_SECS + 100),
            None,
            "the retired key stops verifying once the window elapses"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn activation_refuses_a_fingerprint_that_does_not_match_the_staged_key() {
        let dir = seeded("rotate-wrong-fp", Some(K1));
        stage(&dir, &K2);
        assert!(!activate_staged_key_at(&key_fingerprint(&K1), &dir));
        assert!(!activate_staged_key_at("not-a-fingerprint", &dir));
        assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K1));
        assert!(dir.join("cluster.key.next").exists(), "staged key retained");
        assert_eq!(prev_key_at(&dir, now_unix()), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn activation_with_nothing_staged_changes_nothing() {
        let dir = seeded("rotate-none", Some(K1));
        assert!(!activate_staged_key_at(&key_fingerprint(&K2), &dir));
        assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_staged_file_is_cleared_so_a_later_stage_is_not_blocked() {
        let dir = seeded("rotate-torn", Some(K1));
        fs::write(dir.join("cluster.key.next"), "garbage").unwrap();
        fs::write(dir.join("cluster.key.next.staged_at"), "1").unwrap();
        assert!(!activate_staged_key_at(&key_fingerprint(&K2), &dir));
        assert!(!dir.join("cluster.key.next").exists());
        assert!(!dir.join("cluster.key.next.staged_at").exists());

        stage(&dir, &K2);
        assert!(activate_staged_key_at(&key_fingerprint(&K2), &dir));
        assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K2));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_or_untimestamped_staged_key_cannot_activate() {
        for (tag, age) in [("stale", Some(MAX_STAGE_AGE_SECS + 60)), ("untimed", None)] {
            let dir = seeded(&format!("rotate-{tag}"), Some(K1));
            let staged = dir.join("cluster.key.next");
            assert!(write_key_file(&staged, &K2));
            if let Some(age) = age {
                assert!(write_timestamp_file(
                    &sibling(&staged, ".staged_at"),
                    now_unix() - age
                ));
            }
            assert!(
                !activate_staged_key_at(&key_fingerprint(&K2), &dir),
                "{tag}"
            );
            assert!(!staged.exists(), "{tag}: stale key is cleared, not wedged");
            assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K1));
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_fresh_staged_key_just_inside_the_age_limit_still_activates() {
        let dir = seeded("rotate-edge", Some(K1));
        let staged = dir.join("cluster.key.next");
        assert!(write_key_file(&staged, &K2));
        assert!(write_timestamp_file(
            &sibling(&staged, ".staged_at"),
            now_unix() - (MAX_STAGE_AGE_SECS - 60)
        ));
        assert!(activate_staged_key_at(&key_fingerprint(&K2), &dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_activation_without_a_current_key_leaves_no_prev() {
        let dir = seeded("rotate-first", None);
        stage(&dir, &K2);
        assert!(activate_staged_key_at(&key_fingerprint(&K2), &dir));
        assert_eq!(key_bytes_from_file(&dir.join("cluster.key")), Some(K2));
        assert_eq!(prev_key_at(&dir, now_unix()), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoking_prev_drops_it_immediately_and_is_not_repeatable() {
        let dir = seeded("revoke", Some(K1));
        stage(&dir, &K2);
        assert!(activate_staged_key_at(&key_fingerprint(&K2), &dir));
        assert_eq!(prev_key_at(&dir, now_unix()), Some(K1));

        assert!(revoke_prev_key_at(&dir));
        assert_eq!(prev_key_at(&dir, now_unix()), None);
        assert!(!dir.join("cluster.key.prev").exists());
        assert!(!dir.join("cluster.key.prev.expires_at").exists());
        assert!(!revoke_prev_key_at(&dir), "nothing left to revoke");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_key_requires_an_expiry_record_and_expires_exactly_at_it() {
        let dir = scratch("prev-expiry");
        assert!(write_key_file(&dir.join("cluster.key.prev"), &K1));
        assert_eq!(prev_key_at(&dir, 0), None, "no expiry record -> unusable");

        assert!(write_timestamp_file(
            &dir.join("cluster.key.prev.expires_at"),
            1000
        ));
        assert_eq!(prev_key_at(&dir, 999), Some(K1));
        assert_eq!(prev_key_at(&dir, 1000), None);
        assert_eq!(prev_key_at(&dir, u64::MAX), None);
        let _ = fs::remove_dir_all(&dir);
    }

    fn signer(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_of(sk: &ed25519_dalek::SigningKey) -> String {
        susi_paths::hex::encode(sk.verifying_key().to_bytes())
    }

    fn member_sig(sk: &ed25519_dalek::SigningKey, payload: &str) -> String {
        susi_paths::hex::encode(
            sk.sign(format!("susi-member-v1:{payload}").as_bytes())
                .to_bytes(),
        )
    }

    #[test]
    fn member_verify_accepts_only_a_strict_signature_by_the_named_key() {
        let (alice, mallory) = (signer(7), signer(8));
        let pk = pubkey_of(&alice);
        let sig = member_sig(&alice, "payload");
        assert!(member_verify(&pk, "payload", &sig));

        assert!(!member_verify(&pk, "other payload", &sig));
        assert!(!member_verify(&pubkey_of(&mallory), "payload", &sig));
        let mut tampered = sig.clone();
        tampered.replace_range(0..2, if &sig[0..2] == "00" { "01" } else { "00" });
        assert!(!member_verify(&pk, "payload", &tampered));
        // Malformed encodings are refusals, never passes.
        assert!(!member_verify("zz", "payload", &sig));
        assert!(!member_verify(&pk, "payload", "zz"));
        assert!(!member_verify(&pk[..62], "payload", &sig));
        assert!(!member_verify(&pk, "payload", &sig[..126]));
        assert!(!member_verify("", "payload", ""));
    }

    #[test]
    fn binding_attestation_binds_node_id_and_pubkey_together() {
        let (subject, other) = (signer(11), signer(12));
        let pk = pubkey_of(&subject);
        assert_eq!(bind_payload("n1", &pk), format!("susi-bind-v1:n1:{pk}"));
        let sig = member_sig(&subject, &bind_payload("n1", &pk));
        assert!(verify_bind_attestation("n1", &pk, &sig));

        assert!(!verify_bind_attestation("n2", &pk, &sig), "other node id");
        assert!(
            !verify_bind_attestation("n1", &pubkey_of(&other), &sig),
            "a key the signer does not hold"
        );
        assert!(!verify_bind_attestation("n1", "", &sig));
        assert!(!verify_bind_attestation("n1", &pk, ""));
    }

    #[test]
    fn peer_x25519_public_derives_only_from_a_valid_ed25519_key() {
        let pk = pubkey_of(&signer(21));
        assert!(peer_x25519_public(&pk).is_some());
        assert!(peer_x25519_public("zz").is_none());
        assert!(peer_x25519_public(&pk[..62]).is_none());
        assert!(peer_x25519_public("").is_none());
    }

    #[test]
    fn roster_codec_handles_empty_garbage_and_partial_entries() {
        assert_eq!(encode_roster(&[]), "0");
        assert_eq!(
            encode_roster(&[("a".to_string(), "b:1".to_string())]),
            "6140623a31"
        );
        assert!(decode_roster("0").is_empty());
        assert!(decode_roster("zz").is_empty(), "not hex");
        assert!(decode_roster("ff").is_empty(), "not UTF-8");
        assert!(decode_roster("").is_empty());
        let mixed = susi_paths::hex::encode("a@b;no-separator;c@d:1");
        assert_eq!(
            decode_roster(&mixed),
            [
                ("a".to_string(), "b".to_string()),
                ("c".to_string(), "d:1".to_string())
            ]
        );
        assert!(wire_safe(&encode_roster(&[(
            "n".into(),
            "10.0.0.1:9".into()
        )])));
    }

    fn write_rows(dir: &Path, file: &str, text: &str) {
        fs::write(dir.join(file), text).unwrap();
    }

    #[test]
    fn roster_rows_distinguish_absent_from_damaged_and_pick_up_changes() {
        let dir = scratch("rows");
        assert_eq!(
            config_json_rows_checked_at(&dir, "peers.json").unwrap(),
            Vec::<serde_json::Value>::new(),
            "absent is an empty roster"
        );
        write_rows(&dir, "peers.json", r#"[{"node_id":"a"}]"#);
        assert_eq!(
            config_json_rows_checked_at(&dir, "peers.json")
                .unwrap()
                .len(),
            1
        );
        write_rows(
            &dir,
            "peers.json",
            r#"[{"node_id":"a"},{"node_id":"b"},{"n":1}]"#,
        );
        assert_eq!(
            config_json_rows_checked_at(&dir, "peers.json")
                .unwrap()
                .len(),
            3,
            "a changed file is reloaded, not served stale"
        );

        write_rows(&dir, "object.json", r#"{"not": "an array"}"#);
        write_rows(&dir, "broken.json", "[{");
        for file in ["object.json", "broken.json"] {
            let err = config_json_rows_checked_at(&dir, file).unwrap_err();
            assert!(err.contains(file), "{err}");
            assert!(config_json_rows_at(&dir, file).is_empty(), "lenient form");
            assert!(read_json_rows_strict(&dir.join(file)).is_err());
        }
        assert!(read_json_rows_strict(&dir.join("absent.json"))
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ban_rows_match_on_node_id_or_address_and_a_damaged_list_fails_closed() {
        let row = serde_json::json!({"node_id": "evil", "address": "10.0.0.9:9"});
        assert!(ban_row_matches(&row, "evil", "1.1.1.1:1"));
        assert!(ban_row_matches(&row, "good", "10.0.0.9:9"));
        assert!(!ban_row_matches(&row, "good", "1.1.1.1:1"));
        assert!(!ban_row_matches(
            &serde_json::json!({}),
            "evil",
            "10.0.0.9:9"
        ));
        assert!(!ban_row_matches(
            &serde_json::json!({"node_id": 5}),
            "5",
            "x"
        ));

        let matcher = |r: &serde_json::Value| ban_row_matches(r, "evil", "10.0.0.9:9");

        let absent = scratch("ban-absent");
        assert!(!member_banned_at(&absent, matcher));

        let listed = scratch("ban-listed");
        write_rows(&listed, "peers_banned.json", r#"[{"node_id":"evil"}]"#);
        assert!(member_banned_at(&listed, matcher));

        let clean = scratch("ban-clean");
        write_rows(
            &clean,
            "peers_banned.json",
            r#"[{"node_id":"someone-else"}]"#,
        );
        assert!(!member_banned_at(&clean, matcher));

        for (tag, text) in [("ban-corrupt", "[{"), ("ban-object", r#"{"node_id":"x"}"#)] {
            let dir = scratch(tag);
            write_rows(&dir, "peers_banned.json", text);
            assert!(
                member_banned_at(&dir, |_| false),
                "{tag}: unreadable ban list must refuse membership"
            );
            let _ = fs::remove_dir_all(&dir);
        }
        for dir in [absent, listed, clean] {
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn cached_file_bytes_skips_absent_files_and_follows_rewrites() {
        let dir = scratch("cached");
        let path = dir.join("cred");
        assert_eq!(cached_file_bytes(&path), None, "absent is never cached");
        fs::write(&path, "abc").unwrap();
        assert_eq!(cached_file_bytes(&path).as_deref(), Some(&b"abc"[..]));
        fs::write(&path, "abcdef").unwrap();
        assert_eq!(cached_file_bytes(&path).as_deref(), Some(&b"abcdef"[..]));
        fs::remove_file(&path).unwrap();
        assert_eq!(cached_file_bytes(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Handshake-frame verification. These need the process-global cluster
    /// key location, so they hold the crate's env lock; frames are built by
    /// hand from `mac_tag` so each rejection is attributable to one defect.
    mod frames {
        use super::*;

        struct Home {
            env: Option<susi_paths::test_env::EnvGuard>,
            _guard: std::sync::MutexGuard<'static, ()>,
            tmp: PathBuf,
        }

        impl Drop for Home {
            fn drop(&mut self) {
                self.env = None;
                let _ = fs::remove_dir_all(&self.tmp);
            }
        }

        fn home() -> (Home, [u8; 32]) {
            let guard = crate::env_test_lock();
            let tmp = scratch("frames-home");
            let _ = fs::create_dir_all(tmp.join(".susi"));
            let mut env = susi_paths::test_env::EnvGuard::isolated();
            env.set("HOME", &tmp);
            env.set("USERPROFILE", &tmp);
            env.set("XDG_CONFIG_HOME", tmp.join("xdg"));
            let key = cluster_key().expect("cluster key in isolated home");
            (
                Home {
                    env: Some(env),
                    _guard: guard,
                    tmp,
                },
                key,
            )
        }

        fn ping(key: &[u8; 32], tag: &str, tail: &[&str]) -> String {
            let domain = match tag {
                "SUSI_PING_SIG3" => "ping3",
                "SUSI_PING_SIG2" => "ping2",
                _ => "ping",
            };
            let mut fields = vec![domain];
            fields.extend_from_slice(tail);
            let mac = mac_tag(key, &fields);
            format!("{tag}:{}:{mac}", tail.join(":"))
        }

        const NOW: u64 = 1_800_000_000;

        fn sender() -> std::net::SocketAddr {
            "10.0.0.5:9093".parse().unwrap()
        }

        fn bound(key: &[u8; 32], service: std::net::SocketAddr, ts: u64, nonce: &str) -> String {
            bound_ping_frame(key, "node", "CORE", 7, "00ff", service, ts, nonce).unwrap()
        }

        #[test]
        fn a_fresh_bound_ping_admits_with_the_advertised_service() {
            let mut cache = PingNonceCache::new(8);
            let frame = bound(&KEY, sender(), NOW, "n0nce");
            assert!(frame.starts_with("SUSI_PING_SIG3:node:CORE:7:00ff:"));
            let src: std::net::SocketAddr = "10.0.0.5:51000".parse().unwrap();
            let got = verify_bound_ping_with(&KEY, &frame, src, NOW + 30, &mut cache).unwrap();
            assert_eq!(
                got,
                BoundPing {
                    node_id: "node".into(),
                    caps_csv: "CORE".into(),
                    checksum: 7,
                    bloom_hex: "00ff".into(),
                    service: sender(),
                    ts_secs: NOW,
                    nonce: "n0nce".into(),
                }
            );
            // IPv6 survives the `:`-separated wire through the hex field,
            // and a v4-mapped source matches its v4 advertisement.
            let v6: std::net::SocketAddr = "[fe80::1]:9093".parse().unwrap();
            let src6: std::net::SocketAddr = "[fe80::1]:40000".parse().unwrap();
            let frame6 = bound(&KEY, v6, NOW, "n6");
            let got6 = verify_bound_ping_with(&KEY, &frame6, src6, NOW, &mut cache).unwrap();
            assert_eq!(got6.service, v6);
            let mapped: std::net::SocketAddr = "[::ffff:10.0.0.5]:51000".parse().unwrap();
            let frame_m = bound(&KEY, sender(), NOW, "nm");
            assert!(verify_bound_ping_with(&KEY, &frame_m, mapped, NOW, &mut cache).is_ok());
        }

        #[test]
        fn a_replayed_ping_from_another_source_is_rejected() {
            let mut cache = PingNonceCache::new(8);
            let frame = bound(&KEY, sender(), NOW, "n0nce");
            let replayer: std::net::SocketAddr = "10.0.0.66:51000".parse().unwrap();
            assert_eq!(
                verify_bound_ping_with(&KEY, &frame, replayer, NOW, &mut cache),
                Err(PingRejection::SourceMismatch)
            );
            // The refusal burnt no nonce: the real sender still admits.
            assert!(verify_bound_ping_with(&KEY, &frame, sender(), NOW, &mut cache).is_ok());
            // Re-addressing the frame breaks the MAC.
            let readdressed = frame.replace(
                &susi_paths::hex::encode(sender().to_string()),
                &susi_paths::hex::encode("10.0.0.66:9093"),
            );
            assert_eq!(
                verify_bound_ping_with(&KEY, &readdressed, replayer, NOW, &mut cache),
                Err(PingRejection::BadMac)
            );
        }

        #[test]
        fn a_replayed_nonce_is_rejected_inside_the_window() {
            let mut cache = PingNonceCache::new(8);
            let frame = bound(&KEY, sender(), NOW, "n0nce");
            assert!(verify_bound_ping_with(&KEY, &frame, sender(), NOW, &mut cache).is_ok());
            assert_eq!(
                verify_bound_ping_with(&KEY, &frame, sender(), NOW + 1, &mut cache),
                Err(PingRejection::Replayed)
            );
            // After twice the window the entry may go: the timestamp check
            // refuses the old frame on its own.
            assert_eq!(
                verify_bound_ping_with(
                    &KEY,
                    &frame,
                    sender(),
                    NOW + 3 * PING_WINDOW_SECS,
                    &mut cache
                ),
                Err(PingRejection::Stale)
            );
        }

        #[test]
        fn a_stale_or_future_timestamp_is_rejected() {
            let mut cache = PingNonceCache::new(8);
            let old = bound(&KEY, sender(), NOW - PING_WINDOW_SECS - 1, "old");
            let ahead = bound(&KEY, sender(), NOW + PING_WINDOW_SECS + 1, "ahead");
            let edge = bound(&KEY, sender(), NOW - PING_WINDOW_SECS, "edge");
            for frame in [&old, &ahead] {
                assert_eq!(
                    verify_bound_ping_with(&KEY, frame, sender(), NOW, &mut cache),
                    Err(PingRejection::Stale)
                );
            }
            assert!(verify_bound_ping_with(&KEY, &edge, sender(), NOW, &mut cache).is_ok());
        }

        #[test]
        fn the_nonce_cache_is_bounded_and_fails_closed_when_full() {
            let mut cache = PingNonceCache::new(2);
            assert!(cache.admit("a", NOW).is_ok());
            assert!(cache.admit("b", NOW).is_ok());
            assert_eq!(cache.admit("c", NOW), Err(PingRejection::CacheFull));
            assert_eq!(cache.admit("a", NOW), Err(PingRejection::Replayed));
            // Expired entries are evicted to make room.
            assert!(cache.admit("c", NOW + 2 * PING_WINDOW_SECS + 1).is_ok());
        }

        #[test]
        fn legacy_and_malformed_pings_are_refused() {
            let mut cache = PingNonceCache::new(8);
            let v1 = ping(&KEY, "SUSI_PING_SIG", &["node", "CORE", "7", "00ff", "n1"]);
            let v2 = ping(&KEY, "SUSI_PING_SIG2", &["node", "CORE", "7", "00ff", "n2"]);
            for frame in [&v1, &v2] {
                assert_eq!(
                    verify_bound_ping_with(&KEY, frame, sender(), NOW, &mut cache),
                    Err(PingRejection::Unbound)
                );
            }
            let good = bound(&KEY, sender(), NOW, "n0nce");
            let mut bad_mac = good.clone();
            let last = bad_mac.pop().unwrap();
            bad_mac.push(if last == '0' { '1' } else { '0' });
            let svc = susi_paths::hex::encode(sender().to_string());
            let ts = NOW.to_string();
            let not_an_addr = ping(
                &KEY,
                "SUSI_PING_SIG3",
                &["node", "CORE", "7", "00ff", "7a7a", &ts, "n3"],
            );
            let spaced = ping(
                &KEY,
                "SUSI_PING_SIG3",
                &["node", "CO RE", "7", "00ff", &svc, &ts, "n4"],
            );
            for (label, frame, want) in [
                ("empty", String::new(), PingRejection::Malformed),
                ("tag only", PING_TAG.to_string(), PingRejection::Malformed),
                ("extra field", format!("{good}:x"), PingRejection::Malformed),
                (
                    "pong",
                    "SUSI_PONG_SIG3:a:b".to_string(),
                    PingRejection::Malformed,
                ),
                (
                    "unknown tag",
                    good.replacen(PING_TAG, "SUSI_PING_SIG9", 1),
                    PingRejection::Malformed,
                ),
                ("flipped mac", bad_mac, PingRejection::BadMac),
                (
                    "tampered caps",
                    good.replace(":CORE:", ":GPU:"),
                    PingRejection::BadMac,
                ),
                (
                    "other key",
                    bound(&K1, sender(), NOW, "n5"),
                    PingRejection::BadMac,
                ),
                (
                    "service not an address",
                    not_an_addr,
                    PingRejection::Malformed,
                ),
                ("non wire-safe field", spaced, PingRejection::Malformed),
            ] {
                assert_eq!(
                    verify_bound_ping_with(&KEY, &frame, sender(), NOW, &mut cache),
                    Err(want),
                    "{label}"
                );
            }
            assert!(bound_ping_frame(&KEY, "", "CORE", 7, "00ff", sender(), NOW, "n").is_none());
        }

        #[test]
        fn signed_ping_v3_binds_the_route_source_and_verifies_live() {
            let (_home, _key) = home();
            let dest: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
            assert_eq!(route_source_ip(dest), Some(dest.ip()));
            let (frame, nonce) = signed_ping_v3("CORE", 0, "00", dest).unwrap();
            let src: std::net::SocketAddr = "127.0.0.1:50000".parse().unwrap();
            let got = verify_bound_ping(&frame, src).unwrap();
            assert_eq!((got.node_id, got.nonce), (wire_node_id(), nonce));
            assert_eq!(got.service.ip(), dest.ip());
            assert_eq!(verify_bound_ping(&frame, src), Err(PingRejection::Replayed));
        }

        fn pong(key: &[u8; 32], tag: &str, fields: &[&str]) -> String {
            // fields exclude the tag and MAC; the MAC domain is derived from the tag.
            let domain = match tag {
                "SUSI_PONG_SIG3" => "pong3",
                "SUSI_PONG_SIG2" => "pong2",
                _ => "pong",
            };
            let mut macd = vec![domain];
            macd.extend_from_slice(fields);
            format!("{tag}:{}:{}", fields.join(":"), mac_tag(key, &macd))
        }

        #[test]
        fn pong_verification_binds_nonce_roster_mac_and_field_shape() {
            let (_home, key) = home();
            let roster = encode_roster(&[("p".to_string(), "10.0.0.2:9".to_string())]);
            let fields = ["node", "7", "abcd", "n0nce", roster.as_str()];
            let good = pong(&key, "SUSI_PONG_SIG", &fields);
            let (id, sum, bloom, gossip, pubkey, bind) =
                verify_signed_pong(&good, "n0nce").unwrap();
            assert_eq!((id.as_str(), sum, bloom.as_str()), ("node", 7, "abcd"));
            assert_eq!(gossip, [("p".to_string(), "10.0.0.2:9".to_string())]);
            assert!(pubkey.is_empty() && bind.is_empty());

            assert!(verify_signed_pong(&good, "other-nonce").is_none(), "replay");
            assert!(verify_signed_pong(&good, "").is_none());

            let forged_roster =
                good.replace(&roster, &encode_roster(&[("x".into(), "6.6.6.6:6".into())]));
            let wrong_key = pong(&K1, "SUSI_PONG_SIG", &fields);
            let spaced = pong(
                &key,
                "SUSI_PONG_SIG",
                &["no de", "7", "abcd", "n0nce", roster.as_str()],
            );
            for (label, frame) in [
                ("empty", String::new()),
                (
                    "unknown tag",
                    good.replacen("SUSI_PONG_SIG", "SUSI_PONG_SIG4", 1),
                ),
                ("ping as pong", "SUSI_PING_SIG:a:1:b:c:d".to_string()),
                ("forged roster", forged_roster),
                ("other cluster key", wrong_key),
                ("non wire-safe node id", spaced),
                ("too many fields", format!("{good}:extra")),
                ("too few fields", "SUSI_PONG_SIG:a:1:b".to_string()),
            ] {
                assert!(verify_signed_pong(&frame, "n0nce").is_none(), "{label}");
            }

            // Field counts are per-version: SIG2 needs 8 parts, SIG3 needs 9.
            let sig2_short = good.replacen("SUSI_PONG_SIG", "SUSI_PONG_SIG2", 1);
            let sig3_short = pong(
                &key,
                "SUSI_PONG_SIG2",
                &["node", "pk", "7", "abcd", "n0nce", roster.as_str()],
            )
            .replacen("SUSI_PONG_SIG2", "SUSI_PONG_SIG3", 1);
            assert!(verify_signed_pong(&sig2_short, "n0nce").is_none());
            assert!(verify_signed_pong(&sig3_short, "n0nce").is_none());
        }

        #[test]
        fn v2_pong_carries_a_wire_safe_pubkey_inside_the_mac() {
            let (_home, key) = home();
            let fields = ["node", "pubkeyhex", "7", "abcd", "n0nce", "0"];
            let good = pong(&key, "SUSI_PONG_SIG2", &fields);
            let verified = verify_signed_pong(&good, "n0nce").unwrap();
            assert_eq!(verified.4, "pubkeyhex");
            assert!(verified.5.is_empty(), "v2 has no binding attestation");

            let swapped_pubkey = good.replace("pubkeyhex", "attackerkey");
            assert!(verify_signed_pong(&swapped_pubkey, "n0nce").is_none());
            let spaced = pong(
                &key,
                "SUSI_PONG_SIG2",
                &["node", "pub key", "7", "abcd", "n0nce", "0"],
            );
            assert!(verify_signed_pong(&spaced, "n0nce").is_none());
        }

        #[test]
        fn v3_pong_requires_a_binding_attestation_that_proves_against_its_pubkey() {
            let (_home, key) = home();
            let (subject, other) = (signer(31), signer(32));
            let pk = pubkey_of(&subject);
            let attest = |signer: &ed25519_dalek::SigningKey, node: &str| {
                member_sig(signer, &bind_payload(node, &pk))
            };
            let frame = |node: &str, sig: &str| {
                pong(
                    &key,
                    "SUSI_PONG_SIG3",
                    &[node, &pk, sig, "7", "abcd", "n0nce", "0"],
                )
            };

            let good = frame("node", &attest(&subject, "node"));
            let verified = verify_signed_pong(&good, "n0nce").unwrap();
            assert_eq!(verified.4, pk);
            assert_eq!(verified.5, attest(&subject, "node"));

            // Valid cluster-key MAC throughout; only the attestation is wrong.
            assert!(
                verify_signed_pong(&frame("node", &attest(&other, "node")), "n0nce").is_none(),
                "attestation by a key other than the advertised pubkey"
            );
            assert!(
                verify_signed_pong(&frame("node", &attest(&subject, "someone-else")), "n0nce")
                    .is_none(),
                "attestation for a different node id"
            );
            assert!(
                verify_signed_pong(&frame("node", "deadbeef"), "n0nce").is_none(),
                "malformed attestation"
            );
        }
    }
}
