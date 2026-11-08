//! Sealed peer channel, cryptographic core (`susi-peer-channel-v1`).
//!
//! Every byte that travels between two Susi nodes on different machines
//! travels inside this channel (`docs/peer-channel.md` is the normative
//! description; this module is the implementation it names).
//!
//! The handshake is the Noise pattern `XXpsk0` over X25519, ChaCha20-Poly1305
//! and SHA-256 (Noise Protocol Framework, revision 34), with three Susi
//! bindings:
//!
//! - **Static keys are the node identities.** Each side's static X25519 key
//!   is derived from its Ed25519 `node.key` by the standard birational map
//!   (`cluster_key::node_x25519_secret` / `VerifyingKey::to_montgomery`), and
//!   the encrypted handshake payload carries the Ed25519 public key, the node
//!   id and the node's own binding attestation (`susi-bind-v1`). A receiver
//!   accepts the payload only when the Montgomery form of that Ed25519 key is
//!   exactly the static key the DH tokens authenticated, so the X25519 proof
//!   of possession is a proof of the roster identity.
//! - **The pre-shared key is the cluster membership key.** `psk0` mixes
//!   `HMAC(cluster.key, "susi-peer-channel-v1/psk")` into the key schedule
//!   before the first byte is encrypted. A host without `cluster.key` cannot
//!   produce a first message the responder can open, and learns nothing: the
//!   responder stays silent and never reveals its identity to it.
//! - **Freshness.** The first message's encrypted payload carries a Unix
//!   timestamp; the responder refuses it outside [`WINDOW_SECS`] and refuses
//!   a second first message with the same initiator ephemeral key while that
//!   key could still be inside the window ([`ReplayCache`]). A replayed
//!   handshake could never complete anyway (the replayer lacks the
//!   initiator's ephemeral and static secrets); the cache makes it fail at
//!   the first message instead of costing the responder a DH.
//!
//! After the handshake, each direction has its own `CipherState` with a
//! 64-bit counter nonce, so a replayed, reordered, dropped or injected
//! transport message fails authentication and ends the session.
//!
//! This module is pure: no sockets, no files except the node identity read by
//! [`LocalIdentity::load`] and the cluster key read by [`cluster_psks`].
//! `peer_channel_link` frames these messages onto TCP.

use crate::cluster_key;
use sha2::Digest;

/// Noise protocol name; hashed into the initial handshake hash.
pub const PROTOCOL_NAME: &[u8] = b"Noise_XXpsk0_25519_ChaChaPoly_SHA256";

/// Prologue of a hop-by-hop link session (one TCP connection).
pub const PROLOGUE_LINK: &[u8] = b"susi-peer-channel-v1/link";

/// Prologue of an end-to-end session (one call, possibly relayed).
pub const PROLOGUE_E2E: &[u8] = b"susi-peer-channel-v1/e2e";

/// Label the cluster pre-shared key is derived under.
const PSK_LABEL: &[u8] = b"susi-peer-channel-v1/psk";

/// Largest Noise message (handshake or transport), per the Noise spec.
pub const MAX_MESSAGE: usize = 65_535;

/// Poly1305 tag length.
pub const TAG_LEN: usize = 16;

/// Largest plaintext one transport message carries.
pub const MAX_PLAINTEXT: usize = MAX_MESSAGE - TAG_LEN;

/// Clock skew tolerated on handshake timestamps (the `SUSI_PING_SIG3`
/// window).
pub const WINDOW_SECS: u64 = cluster_key::PING_WINDOW_SECS;

/// Initiator ephemeral keys a responder remembers at once.
pub const REPLAY_CAPACITY: usize = cluster_key::PING_NONCE_CAPACITY;

/// Wire version of the handshake payloads.
const PAYLOAD_VERSION: u8 = 1;

/// Why a handshake or transport step failed. A responder that sees
/// [`ChannelError::BadPsk`] answers nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// Truncated, oversized or structurally invalid message.
    Malformed(&'static str),
    /// The first message does not open under any cluster key this node
    /// holds: the sender does not hold `cluster.key`.
    BadPsk,
    /// An AEAD tag did not verify (tampered, replayed or out of order).
    Decrypt,
    /// The timestamp is outside [`WINDOW_SECS`] of local time.
    Stale,
    /// This initiator ephemeral key was already accepted inside the window.
    Replayed,
    /// The replay cache is full of live entries; refused rather than
    /// forgetting a key that could then replay.
    CacheFull,
    /// The identity payload does not prove the static key it came with.
    BadIdentity(&'static str),
    /// The peer proved an identity, but not the one this side dialed.
    UnexpectedIdentity { got: String },
    /// A Diffie-Hellman output was all zero (a low-order point).
    WeakKey,
    /// No usable `node.key`, or no OS entropy for an ephemeral key.
    NoLocalIdentity,
    /// The counter nonce space of a cipher is exhausted.
    Exhausted,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed channel message: {what}"),
            Self::BadPsk => write!(
                f,
                "handshake does not open under this cluster's key (the peer does not hold cluster.key)"
            ),
            Self::Decrypt => write!(f, "channel message failed authentication"),
            Self::Stale => write!(
                f,
                "handshake timestamp outside the {WINDOW_SECS} s window (check both clocks)"
            ),
            Self::Replayed => write!(f, "replayed handshake refused"),
            Self::CacheFull => write!(f, "handshake replay cache full; refused"),
            Self::BadIdentity(why) => write!(f, "peer identity does not verify: {why}"),
            Self::UnexpectedIdentity { got } => write!(
                f,
                "peer proved node key {got}, not the node key this call expects"
            ),
            Self::WeakKey => write!(f, "low-order Diffie-Hellman key refused"),
            Self::NoLocalIdentity => write!(
                f,
                "this node has no usable node.key (or no OS entropy) to run the peer channel"
            ),
            Self::Exhausted => write!(f, "channel nonce space exhausted"),
        }
    }
}

impl std::error::Error for ChannelError {}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Noise `HKDF(chaining_key, input_key_material, n)` for n = 2 or 3, with
/// HMAC-SHA-256 as `HMAC-HASH`.
fn hkdf(ck: &[u8; 32], ikm: &[u8]) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let temp = cluster_key::hmac_sha256(ck, ikm);
    let out1 = cluster_key::hmac_sha256(&temp, &[0x01]);
    let mut m2 = out1.to_vec();
    m2.push(0x02);
    let out2 = cluster_key::hmac_sha256(&temp, &m2);
    let mut m3 = out2.to_vec();
    m3.push(0x03);
    let out3 = cluster_key::hmac_sha256(&temp, &m3);
    (out1, out2, out3)
}

/// The Noise nonce encoding: 32 zero bits, then the counter little-endian.
fn nonce_bytes(n: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&n.to_le_bytes());
    nonce
}

/// One direction of a Noise `CipherState`: a key and a counter nonce. The
/// key is overwritten when the state is dropped.
pub struct CipherState {
    key: Option<[u8; 32]>,
    n: u64,
}

impl Drop for CipherState {
    fn drop(&mut self) {
        if let Some(key) = self.key.as_mut() {
            key.fill(0);
        }
    }
}

impl CipherState {
    fn empty() -> Self {
        Self { key: None, n: 0 }
    }

    fn with_key(key: [u8; 32]) -> Self {
        Self {
            key: Some(key),
            n: 0,
        }
    }

    /// `ENCRYPT(k, n++, ad, plaintext)`; the plaintext itself while no key
    /// is set (only the very first token of a non-PSK pattern; never in
    /// this protocol, whose first message is already keyed by the PSK).
    pub fn encrypt(&mut self, ad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let Some(key) = self.key else {
            return Ok(plaintext.to_vec());
        };
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(ChannelError::Malformed("plaintext larger than one message"));
        }
        // 2^64 - 1 is reserved by Noise; refuse well before it.
        if self.n == u64::MAX {
            return Err(ChannelError::Exhausted);
        }
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
        let nonce = nonce_bytes(self.n);
        let ct = cipher
            .encrypt(
                chacha20poly1305::Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: ad,
                },
            )
            .map_err(|_| ChannelError::Decrypt)?;
        self.n += 1;
        Ok(ct)
    }

    /// `DECRYPT(k, n++, ad, ciphertext)`. The counter advances only on
    /// success, so a forged message cannot desynchronize a session; the
    /// caller ends the session on any error anyway.
    pub fn decrypt(&mut self, ad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let Some(key) = self.key else {
            return Ok(ciphertext.to_vec());
        };
        if ciphertext.len() < TAG_LEN || ciphertext.len() > MAX_MESSAGE {
            return Err(ChannelError::Malformed("ciphertext length"));
        }
        if self.n == u64::MAX {
            return Err(ChannelError::Exhausted);
        }
        use chacha20poly1305::aead::{Aead, KeyInit, Payload};
        let cipher = chacha20poly1305::ChaCha20Poly1305::new((&key).into());
        let nonce = nonce_bytes(self.n);
        let pt = cipher
            .decrypt(
                chacha20poly1305::Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: ad,
                },
            )
            .map_err(|_| ChannelError::Decrypt)?;
        self.n += 1;
        Ok(pt)
    }
}

/// Noise `SymmetricState`: chaining key, handshake hash and the current
/// handshake cipher.
struct SymmetricState {
    ck: [u8; 32],
    h: [u8; 32],
    cs: CipherState,
}

impl Drop for SymmetricState {
    fn drop(&mut self) {
        self.ck.fill(0);
    }
}

impl SymmetricState {
    /// `InitializeSymmetric(protocol_name)` then `MixHash(prologue)`. The
    /// name is longer than 32 bytes, so `h = SHA-256(name)`.
    fn new(prologue: &[u8]) -> Self {
        let h: [u8; 32] = sha2::Sha256::digest(PROTOCOL_NAME).into();
        let mut ss = Self {
            ck: h,
            h,
            cs: CipherState::empty(),
        };
        ss.mix_hash(prologue);
        ss
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut hasher = sha2::Sha256::new();
        hasher.update(self.h);
        hasher.update(data);
        self.h = hasher.finalize().into();
    }

    fn mix_key(&mut self, ikm: &[u8]) {
        let (ck, k, _) = hkdf(&self.ck, ikm);
        self.ck = ck;
        self.cs = CipherState::with_key(k);
    }

    fn mix_key_and_hash(&mut self, ikm: &[u8]) {
        let (ck, temp_h, k) = hkdf(&self.ck, ikm);
        self.ck = ck;
        self.mix_hash(&temp_h);
        self.cs = CipherState::with_key(k);
    }

    fn encrypt_and_hash(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let ct = self.cs.encrypt(&self.h, plaintext)?;
        self.mix_hash(&ct);
        Ok(ct)
    }

    fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let pt = self.cs.decrypt(&self.h, ciphertext)?;
        self.mix_hash(ciphertext);
        Ok(pt)
    }

    /// `Split()`: the initiator-to-responder and responder-to-initiator
    /// transport ciphers.
    fn split(&self) -> (CipherState, CipherState) {
        let (k1, k2, _) = hkdf(&self.ck, &[]);
        (CipherState::with_key(k1), CipherState::with_key(k2))
    }
}

/// Run X25519 and refuse an all-zero (non-contributory) output.
fn dh(
    secret: &x25519_dalek::StaticSecret,
    public: &x25519_dalek::PublicKey,
) -> Result<[u8; 32], ChannelError> {
    let shared = secret.diffie_hellman(public);
    if !shared.was_contributory() {
        return Err(ChannelError::WeakKey);
    }
    Ok(*shared.as_bytes())
}

fn fresh_ephemeral() -> Result<x25519_dalek::StaticSecret, ChannelError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| ChannelError::NoLocalIdentity)?;
    let secret = x25519_dalek::StaticSecret::from(seed);
    seed.fill(0);
    Ok(secret)
}

/// The identity a node proves in the handshake.
pub struct LocalIdentity {
    x_secret: x25519_dalek::StaticSecret,
    x_public: x25519_dalek::PublicKey,
    node_id: String,
    ed_public: [u8; 32],
    bind_sig: [u8; 64],
}

impl LocalIdentity {
    /// This node's identity from `node.key` and `node_id`. `None` without a
    /// usable node key.
    pub fn load() -> Option<Self> {
        let x_secret = cluster_key::node_x25519_secret()?;
        let ed_hex = cluster_key::node_pubkey_hex()?;
        let sig_hex = cluster_key::bind_attestation()?;
        let ed_public = <[u8; 32]>::try_from(hex::decode(&ed_hex).ok()?.as_slice()).ok()?;
        let bind_sig = <[u8; 64]>::try_from(hex::decode(&sig_hex).ok()?.as_slice()).ok()?;
        let x_public = x25519_dalek::PublicKey::from(&x_secret);
        Some(Self {
            x_secret,
            x_public,
            node_id: cluster_key::wire_node_id(),
            ed_public,
            bind_sig,
        })
    }

    /// An identity from an Ed25519 seed, the way `node.key` holds one
    /// (tests, and tools that hold a key outside the config directory).
    pub fn from_seed(seed: &[u8; 32], node_id: &str) -> Self {
        let sk = ed25519_dalek::SigningKey::from_bytes(seed);
        let ed_public = sk.verifying_key().to_bytes();
        let digest = sha2::Sha512::digest(seed);
        let mut x = [0u8; 32];
        x.copy_from_slice(&digest[..32]);
        let x_secret = x25519_dalek::StaticSecret::from(x);
        x.fill(0);
        let x_public = x25519_dalek::PublicKey::from(&x_secret);
        let payload = format!(
            "susi-member-v1:{}",
            cluster_key::bind_payload(node_id, &hex::encode(ed_public))
        );
        let bind_sig = ed25519_dalek::Signer::sign(&sk, payload.as_bytes()).to_bytes();
        Self {
            x_secret,
            x_public,
            node_id: node_id.to_string(),
            ed_public,
            bind_sig,
        }
    }

    /// The node id this identity claims.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// The Ed25519 public key, hex, as rosters store it.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.ed_public)
    }

    /// `ts | id_len | node_id | ed25519 pubkey | bind_sig`, version-prefixed.
    fn payload(&self, now: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 8 + 1 + self.node_id.len() + 32 + 64);
        out.push(PAYLOAD_VERSION);
        out.extend_from_slice(&now.to_be_bytes());
        out.push(u8::try_from(self.node_id.len()).unwrap_or(0));
        out.extend_from_slice(self.node_id.as_bytes());
        out.extend_from_slice(&self.ed_public);
        out.extend_from_slice(&self.bind_sig);
        out
    }
}

/// An identity the other side proved: its node id and Ed25519 public key
/// (hex), exactly as `peers.json` binds them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerIdentity {
    pub node_id: String,
    pub pubkey: String,
}

/// What the side that dials requires of the identity it reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Any identity that proves the cluster key (first contact; the caller
    /// decides what to do with the identity it learns).
    AnyMember,
    /// Exactly this Ed25519 public key (hex).
    Key(String),
}

/// Node ids travel in the wire alphabet `cluster_key` uses everywhere.
fn node_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ','))
}

/// Parse and verify an identity payload against the static key `rs` the
/// handshake's DH tokens authenticated.
fn verify_identity(
    payload: &[u8],
    rs: &x25519_dalek::PublicKey,
    now: u64,
) -> Result<PeerIdentity, ChannelError> {
    let (&version, rest) = payload
        .split_first()
        .ok_or(ChannelError::Malformed("empty identity payload"))?;
    if version != PAYLOAD_VERSION {
        return Err(ChannelError::Malformed("identity payload version"));
    }
    let (ts, rest) = rest
        .split_first_chunk::<8>()
        .ok_or(ChannelError::Malformed("identity timestamp"))?;
    if now.abs_diff(u64::from_be_bytes(*ts)) > WINDOW_SECS {
        return Err(ChannelError::Stale);
    }
    let (&id_len, rest) = rest
        .split_first()
        .ok_or(ChannelError::Malformed("identity node id length"))?;
    let id_len = usize::from(id_len);
    if rest.len() != id_len + 32 + 64 {
        return Err(ChannelError::Malformed("identity payload length"));
    }
    let (id, rest) = rest.split_at(id_len);
    let node_id = std::str::from_utf8(id)
        .ok()
        .filter(|id| node_id_ok(id))
        .ok_or(ChannelError::BadIdentity(
            "node id outside the wire alphabet",
        ))?;
    let (ed, sig) = rest.split_at(32);
    let ed: [u8; 32] = ed
        .try_into()
        .map_err(|_| ChannelError::Malformed("identity key"))?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&ed)
        .map_err(|_| ChannelError::BadIdentity("not an Ed25519 public key"))?;
    if vk.is_weak() {
        return Err(ChannelError::BadIdentity("weak Ed25519 key"));
    }
    if vk.to_montgomery().to_bytes() != *rs.as_bytes() {
        return Err(ChannelError::BadIdentity(
            "Ed25519 key does not match the authenticated static key",
        ));
    }
    let pubkey = hex::encode(ed);
    if !cluster_key::verify_bind_attestation(node_id, &pubkey, &hex::encode(sig)) {
        return Err(ChannelError::BadIdentity("binding attestation"));
    }
    Ok(PeerIdentity {
        node_id: node_id.to_string(),
        pubkey,
    })
}

/// The pre-shared keys derived from the cluster keys this node holds: the
/// current key first, then the previous key while a re-key's overlap
/// window keeps it valid. Empty without `cluster.key`.
pub fn cluster_psks() -> Vec<[u8; 32]> {
    [cluster_key::cluster_key(), cluster_key::prev_key()]
        .into_iter()
        .flatten()
        .map(|k| psk_from_cluster_key(&k))
        .collect()
}

/// `HMAC-SHA-256(cluster.key, "susi-peer-channel-v1/psk")`.
pub fn psk_from_cluster_key(cluster_key: &[u8; 32]) -> [u8; 32] {
    cluster_key::hmac_sha256(cluster_key, PSK_LABEL)
}

fn read_public(bytes: &[u8]) -> Result<x25519_dalek::PublicKey, ChannelError> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ChannelError::Malformed("public key length"))?;
    Ok(x25519_dalek::PublicKey::from(arr))
}

/// Split a fixed-length prefix off a message.
fn take(msg: &[u8], n: usize) -> Result<(&[u8], &[u8]), ChannelError> {
    if msg.len() < n {
        return Err(ChannelError::Malformed("message shorter than its tokens"));
    }
    Ok(msg.split_at(n))
}

/// The initiator between its first message and the responder's answer.
pub struct Initiator {
    ss: SymmetricState,
    e: x25519_dalek::StaticSecret,
    expect: Expect,
}

/// Message 1 (`-> psk, e`) plus the state that reads message 2. `flags`
/// travels encrypted in the first payload (see `peer_channel_link`).
pub fn initiator_start(
    psk: &[u8; 32],
    prologue: &[u8],
    flags: u8,
    expect: Expect,
) -> Result<(Initiator, Vec<u8>), ChannelError> {
    initiator_start_at(psk, prologue, flags, expect, now_secs())
}

/// [`initiator_start`] at an explicit clock reading.
pub fn initiator_start_at(
    psk: &[u8; 32],
    prologue: &[u8],
    flags: u8,
    expect: Expect,
    now: u64,
) -> Result<(Initiator, Vec<u8>), ChannelError> {
    let mut ss = SymmetricState::new(prologue);
    let e = fresh_ephemeral()?;
    let e_pub = x25519_dalek::PublicKey::from(&e);
    let mut msg = Vec::with_capacity(32 + 10 + TAG_LEN);
    // psk
    ss.mix_key_and_hash(psk);
    // e (PSK handshake: also MixKey(e.public_key))
    msg.extend_from_slice(e_pub.as_bytes());
    ss.mix_hash(e_pub.as_bytes());
    ss.mix_key(e_pub.as_bytes());
    let mut payload = vec![PAYLOAD_VERSION];
    payload.extend_from_slice(&now.to_be_bytes());
    payload.push(flags);
    msg.extend_from_slice(&ss.encrypt_and_hash(&payload)?);
    Ok((Initiator { ss, e, expect }, msg))
}

impl Initiator {
    /// Read message 2 (`<- e, ee, s, es`), verify the responder's identity,
    /// and write message 3 (`-> s, se`). Returns message 3, the transport
    /// ciphers and the identity the responder proved.
    pub fn finish(
        self,
        local: &LocalIdentity,
        msg2: &[u8],
    ) -> Result<(Vec<u8>, Transport, PeerIdentity), ChannelError> {
        self.finish_at(local, msg2, now_secs())
    }

    /// [`Initiator::finish`] at an explicit clock reading.
    pub fn finish_at(
        mut self,
        local: &LocalIdentity,
        msg2: &[u8],
        now: u64,
    ) -> Result<(Vec<u8>, Transport, PeerIdentity), ChannelError> {
        if msg2.len() > MAX_MESSAGE {
            return Err(ChannelError::Malformed("message 2 too long"));
        }
        // e
        let (re, rest) = take(msg2, 32)?;
        let re = read_public(re)?;
        self.ss.mix_hash(re.as_bytes());
        self.ss.mix_key(re.as_bytes());
        // ee
        self.ss.mix_key(&dh(&self.e, &re)?);
        // s
        let (rs_ct, rest) = take(rest, 32 + TAG_LEN)?;
        let rs = read_public(&self.ss.decrypt_and_hash(rs_ct)?)?;
        // es (initiator: DH(e, rs))
        self.ss.mix_key(&dh(&self.e, &rs)?);
        let payload = self.ss.decrypt_and_hash(rest)?;
        let peer = verify_identity(&payload, &rs, now)?;
        if let Expect::Key(want) = &self.expect {
            if !want.eq_ignore_ascii_case(&peer.pubkey) {
                return Err(ChannelError::UnexpectedIdentity { got: peer.pubkey });
            }
        }
        // s
        let mut msg3 = self.ss.encrypt_and_hash(local.x_public.as_bytes())?;
        // se (initiator: DH(s, re))
        self.ss.mix_key(&dh(&local.x_secret, &re)?);
        msg3.extend_from_slice(&self.ss.encrypt_and_hash(&local.payload(now))?);
        let (send, recv) = self.ss.split();
        Ok((
            msg3,
            Transport {
                send,
                recv,
                handshake_hash: self.ss.h,
            },
            peer,
        ))
    }
}

/// Initiator ephemeral keys seen in accepted first messages.
pub type ReplayCache = cluster_key::PingNonceCache;

/// A new replay cache sized for one listener.
pub fn new_replay_cache() -> ReplayCache {
    ReplayCache::new(REPLAY_CAPACITY)
}

/// The responder between its answer and the initiator's last message.
pub struct Responder {
    ss: SymmetricState,
    e: x25519_dalek::StaticSecret,
}

/// Read message 1 under each candidate PSK, check its freshness, record its
/// ephemeral key, and write message 2. Returns the state that reads
/// message 3, message 2, and the initiator's flags byte.
pub fn responder_accept(
    local: &LocalIdentity,
    psks: &[[u8; 32]],
    prologue: &[u8],
    msg1: &[u8],
    replay: &mut ReplayCache,
) -> Result<(Responder, Vec<u8>, u8), ChannelError> {
    responder_accept_at(local, psks, prologue, msg1, replay, now_secs())
}

/// [`responder_accept`] at an explicit clock reading.
#[allow(clippy::too_many_arguments)] // the five handshake inputs plus the clock tests pin
pub fn responder_accept_at(
    local: &LocalIdentity,
    psks: &[[u8; 32]],
    prologue: &[u8],
    msg1: &[u8],
    replay: &mut ReplayCache,
    now: u64,
) -> Result<(Responder, Vec<u8>, u8), ChannelError> {
    if msg1.len() > MAX_MESSAGE {
        return Err(ChannelError::Malformed("message 1 too long"));
    }
    let (re_bytes, payload_ct) = take(msg1, 32)?;
    let re = read_public(re_bytes)?;
    let mut opened = None;
    for psk in psks {
        let mut ss = SymmetricState::new(prologue);
        ss.mix_key_and_hash(psk);
        ss.mix_hash(re.as_bytes());
        ss.mix_key(re.as_bytes());
        if let Ok(payload) = ss.decrypt_and_hash(payload_ct) {
            opened = Some((ss, payload));
            break;
        }
    }
    let (mut ss, payload) = opened.ok_or(ChannelError::BadPsk)?;
    if payload.len() != 10 || payload[0] != PAYLOAD_VERSION {
        return Err(ChannelError::Malformed("message 1 payload"));
    }
    let mut ts = [0u8; 8];
    ts.copy_from_slice(&payload[1..9]);
    if now.abs_diff(u64::from_be_bytes(ts)) > WINDOW_SECS {
        return Err(ChannelError::Stale);
    }
    let flags = payload[9];
    replay
        .admit(&hex::encode(re.as_bytes()), now)
        .map_err(|rejection| match rejection {
            cluster_key::PingRejection::CacheFull => ChannelError::CacheFull,
            cluster_key::PingRejection::Replayed
            | cluster_key::PingRejection::NoKey
            | cluster_key::PingRejection::Unbound
            | cluster_key::PingRejection::Malformed
            | cluster_key::PingRejection::BadMac
            | cluster_key::PingRejection::SourceMismatch
            | cluster_key::PingRejection::Stale => ChannelError::Replayed,
        })?;
    let e = fresh_ephemeral()?;
    let e_pub = x25519_dalek::PublicKey::from(&e);
    // e
    let mut msg2 = e_pub.as_bytes().to_vec();
    ss.mix_hash(e_pub.as_bytes());
    ss.mix_key(e_pub.as_bytes());
    // ee
    ss.mix_key(&dh(&e, &re)?);
    // s
    msg2.extend_from_slice(&ss.encrypt_and_hash(local.x_public.as_bytes())?);
    // es (responder: DH(s, re))
    ss.mix_key(&dh(&local.x_secret, &re)?);
    msg2.extend_from_slice(&ss.encrypt_and_hash(&local.payload(now))?);
    Ok((Responder { ss, e }, msg2, flags))
}

impl Responder {
    /// Read message 3 (`-> s, se`) and verify the initiator's identity.
    pub fn finish(self, msg3: &[u8]) -> Result<(Transport, PeerIdentity), ChannelError> {
        self.finish_at(msg3, now_secs())
    }

    /// [`Responder::finish`] at an explicit clock reading.
    pub fn finish_at(
        mut self,
        msg3: &[u8],
        now: u64,
    ) -> Result<(Transport, PeerIdentity), ChannelError> {
        if msg3.len() > MAX_MESSAGE {
            return Err(ChannelError::Malformed("message 3 too long"));
        }
        // s
        let (rs_ct, rest) = take(msg3, 32 + TAG_LEN)?;
        let rs = read_public(&self.ss.decrypt_and_hash(rs_ct)?)?;
        // se (responder: DH(e, rs))
        self.ss.mix_key(&dh(&self.e, &rs)?);
        let payload = self.ss.decrypt_and_hash(rest)?;
        let peer = verify_identity(&payload, &rs, now)?;
        // Responder's transport: it sends with the second cipher.
        let (to_responder, to_initiator) = self.ss.split();
        Ok((
            Transport {
                send: to_initiator,
                recv: to_responder,
                handshake_hash: self.ss.h,
            },
            peer,
        ))
    }
}

/// The two transport ciphers of an established session.
pub struct Transport {
    send: CipherState,
    recv: CipherState,
    handshake_hash: [u8; 32],
}

impl Transport {
    /// Seal one transport message (at most [`MAX_PLAINTEXT`] bytes).
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        self.send.encrypt(&[], plaintext)
    }

    /// Open the next transport message from the other side.
    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        self.recv.decrypt(&[], ciphertext)
    }

    /// The final handshake hash: identical on both sides of one session and
    /// unique to it (channel binding).
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.handshake_hash
    }

    /// Split into the sending and the receiving cipher, so two threads can
    /// own one direction each.
    pub fn into_halves(self) -> (CipherState, CipherState) {
        (self.send, self.recv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn ident(byte: u8, id: &str) -> LocalIdentity {
        LocalIdentity::from_seed(&[byte; 32], id)
    }

    fn psk(byte: u8) -> [u8; 32] {
        psk_from_cluster_key(&[byte; 32])
    }

    /// Full handshake; returns both transports and both proven identities.
    fn handshake(
        a: &LocalIdentity,
        b: &LocalIdentity,
        expect: Expect,
    ) -> Result<(Transport, PeerIdentity, Transport, PeerIdentity), ChannelError> {
        let mut cache = new_replay_cache();
        let (init, m1) = initiator_start_at(&psk(7), PROLOGUE_LINK, 3, expect, NOW)?;
        let (resp, m2, flags) =
            responder_accept_at(b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW)?;
        assert_eq!(flags, 3);
        let (m3, ta, peer_b) = init.finish_at(a, &m2, NOW)?;
        let (tb, peer_a) = resp.finish_at(&m3, NOW)?;
        Ok((ta, peer_b, tb, peer_a))
    }

    #[test]
    fn handshake_authenticates_both_identities_and_keys_agree() {
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        let (mut ta, peer_b, mut tb, peer_a) =
            handshake(&a, &b, Expect::Key(b.pubkey_hex())).unwrap();
        assert_eq!(peer_b.node_id, "susi-node-b");
        assert_eq!(peer_b.pubkey, b.pubkey_hex());
        assert_eq!(peer_a.node_id, "susi-node-a");
        assert_eq!(peer_a.pubkey, a.pubkey_hex());
        assert_eq!(ta.handshake_hash(), tb.handshake_hash());
        let ct = ta.seal(b"POST /mcp HTTP/1.1").unwrap();
        assert!(!ct.windows(4).any(|w| w == b"POST"));
        assert_eq!(tb.open(&ct).unwrap(), b"POST /mcp HTTP/1.1");
        let back = tb.seal(b"HTTP/1.1 200 OK").unwrap();
        assert_eq!(ta.open(&back).unwrap(), b"HTTP/1.1 200 OK");
    }

    #[test]
    fn a_host_without_the_cluster_key_gets_no_answer() {
        let b = ident(2, "susi-node-b");
        let mut cache = new_replay_cache();
        let (_, m1) =
            initiator_start_at(&psk(9), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        let err = responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW)
            .err()
            .unwrap();
        assert_eq!(err, ChannelError::BadPsk);
    }

    #[test]
    fn the_previous_cluster_key_still_opens_during_a_rekey_overlap() {
        let b = ident(2, "susi-node-b");
        let mut cache = new_replay_cache();
        let (_, m1) =
            initiator_start_at(&psk(9), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        assert!(
            responder_accept_at(&b, &[psk(7), psk(9)], PROLOGUE_LINK, &m1, &mut cache, NOW).is_ok()
        );
    }

    #[test]
    fn a_replayed_first_message_is_refused() {
        let b = ident(2, "susi-node-b");
        let mut cache = new_replay_cache();
        let (_, m1) =
            initiator_start_at(&psk(7), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        assert!(responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW).is_ok());
        let err = responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW + 5)
            .err()
            .unwrap();
        assert_eq!(err, ChannelError::Replayed);
    }

    #[test]
    fn a_stale_first_message_is_refused() {
        let b = ident(2, "susi-node-b");
        let mut cache = new_replay_cache();
        let (_, m1) =
            initiator_start_at(&psk(7), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        let err = responder_accept_at(
            &b,
            &[psk(7)],
            PROLOGUE_LINK,
            &m1,
            &mut cache,
            NOW + WINDOW_SECS + 1,
        )
        .err()
        .unwrap();
        assert_eq!(err, ChannelError::Stale);
    }

    #[test]
    fn a_wrong_identity_is_refused_by_the_dialer() {
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        let c = ident(3, "susi-node-c");
        let err = handshake(&a, &b, Expect::Key(c.pubkey_hex()))
            .err()
            .unwrap();
        assert_eq!(
            err,
            ChannelError::UnexpectedIdentity {
                got: b.pubkey_hex()
            }
        );
    }

    #[test]
    fn prologues_separate_link_and_end_to_end_sessions() {
        let b = ident(2, "susi-node-b");
        let mut cache = new_replay_cache();
        let (_, m1) = initiator_start_at(&psk(7), PROLOGUE_E2E, 0, Expect::AnyMember, NOW).unwrap();
        assert_eq!(
            responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW)
                .err()
                .unwrap(),
            ChannelError::BadPsk
        );
    }

    #[test]
    fn tampering_with_any_handshake_message_fails() {
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        // Message 2 tampered: the initiator refuses.
        let mut cache = new_replay_cache();
        let (init, m1) =
            initiator_start_at(&psk(7), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        let (_, mut m2, _) =
            responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW).unwrap();
        let last = m2.len() - 1;
        m2[last] ^= 1;
        assert!(init.finish_at(&a, &m2, NOW).is_err());
        // Message 3 tampered: the responder refuses.
        let (init, m1) =
            initiator_start_at(&psk(7), PROLOGUE_LINK, 0, Expect::AnyMember, NOW).unwrap();
        let (resp, m2, _) =
            responder_accept_at(&b, &[psk(7)], PROLOGUE_LINK, &m1, &mut cache, NOW).unwrap();
        let (mut m3, _, _) = init.finish_at(&a, &m2, NOW).unwrap();
        m3[40] ^= 1;
        assert!(resp.finish_at(&m3, NOW).is_err());
    }

    #[test]
    fn a_forged_identity_payload_does_not_verify() {
        // C runs the handshake with its own static key but claims B's node
        // id and Ed25519 key: the Montgomery check refuses it.
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        let mut forged = ident(3, "susi-node-b");
        forged.ed_public = b.ed_public;
        forged.bind_sig = b.bind_sig;
        let err = handshake(&a, &forged, Expect::AnyMember).err().unwrap();
        assert_eq!(
            err,
            ChannelError::BadIdentity("Ed25519 key does not match the authenticated static key")
        );
    }

    #[test]
    fn replayed_or_reordered_transport_messages_fail() {
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        let (mut ta, _, mut tb, _) = handshake(&a, &b, Expect::AnyMember).unwrap();
        let first = ta.seal(b"one").unwrap();
        let second = ta.seal(b"two").unwrap();
        assert_eq!(tb.open(&second).err().unwrap(), ChannelError::Decrypt);
        assert_eq!(tb.open(&first).unwrap(), b"one");
        assert_eq!(tb.open(&first).err().unwrap(), ChannelError::Decrypt);
        assert_eq!(tb.open(&second).unwrap(), b"two");
    }

    #[test]
    fn sessions_have_independent_keys() {
        let a = ident(1, "susi-node-a");
        let b = ident(2, "susi-node-b");
        let (mut ta1, _, _, _) = handshake(&a, &b, Expect::AnyMember).unwrap();
        let (_, _, mut tb2, _) = handshake(&a, &b, Expect::AnyMember).unwrap();
        let ct = ta1.seal(b"cross-session").unwrap();
        assert_eq!(tb2.open(&ct).err().unwrap(), ChannelError::Decrypt);
    }

    #[test]
    fn noise_hkdf_matches_rfc5869_shape() {
        // HKDF with HMAC-SHA-256: output 1 = HMAC(temp, 0x01), output 2 =
        // HMAC(temp, out1 || 0x02) — check against a direct computation.
        let ck = [0x11u8; 32];
        let (o1, o2, o3) = hkdf(&ck, b"ikm");
        let temp = cluster_key::hmac_sha256(&ck, b"ikm");
        assert_eq!(o1, cluster_key::hmac_sha256(&temp, &[1]));
        let mut m = o1.to_vec();
        m.push(2);
        assert_eq!(o2, cluster_key::hmac_sha256(&temp, &m));
        let mut m = o2.to_vec();
        m.push(3);
        assert_eq!(o3, cluster_key::hmac_sha256(&temp, &m));
    }
}
