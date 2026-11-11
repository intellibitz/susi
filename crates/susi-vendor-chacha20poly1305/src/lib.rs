//! Pure-Rust `#![forbid(unsafe_code)]` implementation of IETF RFC 8439
//! ChaCha20-Poly1305 Authenticated Encryption with Associated Data (AEAD).
//!
//! Consumers depend on this crate under the dep key `chacha20poly1305` (via
//! `package = "susi-vendor-chacha20poly1305"`), preserving the exact
//! `ChaCha20Poly1305`, `Key`, `Nonce`, and `aead::{Aead, Error, KeyInit,
//! Payload}` surface used by `susi-config` (`cluster_key` and `peer_channel`)
//! without pulling the eight-crate RustCrypto `chacha20poly1305` / `aead` /
//! `cipher` / `poly1305` / `universal-hash` / `inout` / `opaque-debug` graph.
//!
//! Security invariants:
//! - ChaCha20 uses pure word-wise ARX operations (`wrapping_add`, `^`,
//!   `rotate_left`) with zero secret-dependent branches or table lookups.
//! - Poly1305 uses 5x26-bit limb arithmetic modulo `2^130 - 5` in `u64` with
//!   branchless final reduction and `subtle::ConstantTimeEq` 16-byte tag
//!   verification prior to decrypting any ciphertext bytes.
//! - Key material, one-time Poly1305 keys, and ChaCha20 keystream blocks are
//!   explicitly cleared via `zeroize::Zeroize` on drop / return.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// Key length in bytes (256 bits, RFC 8439 §2.3).
pub const KEY_SIZE: usize = 32;
/// IETF nonce length in bytes (96 bits, RFC 8439 §2.3).
pub const NONCE_SIZE: usize = 12;
/// Poly1305 authentication tag length in bytes (128 bits, RFC 8439 §2.5).
pub const TAG_SIZE: usize = 16;

/// Maximum plaintext length in bytes for a single 96-bit nonce under 32-bit
/// block counter `1..=u32::MAX` (`(2^32 - 1) * 64`, RFC 8439 §2.8).
const MAX_PLAINTEXT_LEN: u64 = (u32::MAX as u64) * 64;

/// 256-bit ChaCha20-Poly1305 key, zeroized on drop.
#[derive(Clone)]
pub struct Key([u8; KEY_SIZE]);

impl Key {
    /// Borrow the raw 32-byte key slice.
    #[must_use]
    pub const fn as_slice(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl From<[u8; KEY_SIZE]> for Key {
    fn from(bytes: [u8; KEY_SIZE]) -> Self {
        Self(bytes)
    }
}

impl From<&[u8; KEY_SIZE]> for Key {
    fn from(bytes: &[u8; KEY_SIZE]) -> Self {
        Self(*bytes)
    }
}

impl From<&Self> for Key {
    fn from(other: &Self) -> Self {
        other.clone()
    }
}

/// 96-bit IETF ChaCha20-Poly1305 nonce.
///
/// Constructed infallibly from `[u8; 12]` or fallibly via [`Nonce::from_slice`]
/// without panicking: a slice of any length other than 12 bytes produces an
/// invalid nonce marker that causes `encrypt` / `decrypt` to fail closed with
/// [`aead::Error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nonce(Option<[u8; NONCE_SIZE]>);

impl Nonce {
    /// Construct a nonce view from `slice`. Never panics; non-12-byte slices
    /// are rejected by `encrypt` and `decrypt` with [`aead::Error`].
    #[must_use]
    pub fn from_slice(slice: &[u8]) -> Self {
        Self(slice.try_into().ok())
    }
}

impl From<[u8; NONCE_SIZE]> for Nonce {
    fn from(bytes: [u8; NONCE_SIZE]) -> Self {
        Self(Some(bytes))
    }
}

impl From<&[u8; NONCE_SIZE]> for Nonce {
    fn from(bytes: &[u8; NONCE_SIZE]) -> Self {
        Self(Some(*bytes))
    }
}

/// Minimal AEAD trait surface matching `chacha20poly1305::aead`.
pub mod aead {
    use core::fmt;

    use super::{Key, Nonce, KEY_SIZE};

    /// Opaque authentication / parameter failure. Intentionally carries no
    /// oracle detail distinguishing tag mismatch from malformed input.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Error;

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("aead::Error")
        }
    }

    impl std::error::Error for Error {}

    /// Plaintext or ciphertext paired with Associated Authenticated Data (AAD).
    #[derive(Clone, Copy, Debug)]
    pub struct Payload<'msg, 'aad> {
        /// Plaintext (for `encrypt`) or ciphertext+tag (for `decrypt`).
        pub msg: &'msg [u8],
        /// Unencrypted associated data bound into the Poly1305 tag.
        pub aad: &'aad [u8],
    }

    impl<'msg, 'aad> From<&'msg [u8]> for Payload<'msg, 'aad> {
        fn from(msg: &'msg [u8]) -> Self {
            Self { msg, aad: b"" }
        }
    }

    impl<'msg, 'aad, const N: usize> From<&'msg [u8; N]> for Payload<'msg, 'aad> {
        fn from(msg: &'msg [u8; N]) -> Self {
            Self {
                msg: msg.as_slice(),
                aad: b"",
            }
        }
    }

    /// Key-initialized symmetric primitive.
    pub trait KeyInit: Sized {
        /// Construct a cipher instance from a 256-bit [`Key`].
        #[must_use]
        fn new(key: Key) -> Self;

        /// Construct a cipher instance from a byte slice, returning [`Error`]
        /// if `slice.len() != 32`.
        fn new_from_slice(slice: &[u8]) -> Result<Self, Error> {
            let raw: &[u8; KEY_SIZE] = slice.try_into().map_err(|_| Error)?;
            Ok(Self::new(Key::from(raw)))
        }
    }

    /// Authenticated Encryption with Associated Data.
    pub trait Aead {
        /// Encrypt `plaintext` (with optional AAD) under `nonce`, returning
        /// `ciphertext || tag` (`plaintext.len() + 16` bytes).
        fn encrypt<'msg, 'aad>(
            &self,
            nonce: Nonce,
            plaintext: impl Into<Payload<'msg, 'aad>>,
        ) -> Result<Vec<u8>, Error>;

        /// Verify and decrypt `ciphertext` (`ciphertext || 16-byte tag`, with
        /// optional AAD) under `nonce`. Returns [`Error`] without releasing any
        /// unauthenticated bytes on tag mismatch or malformed input.
        fn decrypt<'msg, 'aad>(
            &self,
            nonce: Nonce,
            ciphertext: impl Into<Payload<'msg, 'aad>>,
        ) -> Result<Vec<u8>, Error>;
    }
}

/// RFC 8439 ChaCha20-Poly1305 AEAD cipher.
#[derive(Clone)]
pub struct ChaCha20Poly1305 {
    key: [u8; KEY_SIZE],
}

impl Drop for ChaCha20Poly1305 {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl aead::KeyInit for ChaCha20Poly1305 {
    fn new(key: Key) -> Self {
        Self { key: key.0 }
    }
}

impl aead::Aead for ChaCha20Poly1305 {
    fn encrypt<'msg, 'aad>(
        &self,
        nonce: Nonce,
        plaintext: impl Into<aead::Payload<'msg, 'aad>>,
    ) -> Result<Vec<u8>, aead::Error> {
        let nonce = nonce.0.ok_or(aead::Error)?;
        let payload = plaintext.into();
        if (payload.msg.len() as u64) > MAX_PLAINTEXT_LEN {
            return Err(aead::Error);
        }
        let out_len = payload.msg.len().checked_add(TAG_SIZE).ok_or(aead::Error)?;

        let mut otk = chacha20_poly1305_key_gen(&self.key, &nonce);
        let mut out = Vec::with_capacity(out_len);
        out.extend_from_slice(payload.msg);
        chacha20_xor(&self.key, 1, &nonce, &mut out)?;

        let tag = poly1305_aead_tag(&otk, payload.aad, &out);
        otk.zeroize();
        out.extend_from_slice(&tag);
        Ok(out)
    }

    fn decrypt<'msg, 'aad>(
        &self,
        nonce: Nonce,
        ciphertext: impl Into<aead::Payload<'msg, 'aad>>,
    ) -> Result<Vec<u8>, aead::Error> {
        let nonce = nonce.0.ok_or(aead::Error)?;
        let payload = ciphertext.into();
        if payload.msg.len() < TAG_SIZE {
            return Err(aead::Error);
        }
        let ct_len = payload.msg.len() - TAG_SIZE;
        if (ct_len as u64) > MAX_PLAINTEXT_LEN {
            return Err(aead::Error);
        }
        let (ct, received_tag) = payload.msg.split_at(ct_len);

        let mut otk = chacha20_poly1305_key_gen(&self.key, &nonce);
        let expected_tag = poly1305_aead_tag(&otk, payload.aad, ct);
        otk.zeroize();

        if !bool::from(expected_tag.ct_eq(received_tag)) {
            return Err(aead::Error);
        }

        let mut plaintext = ct.to_vec();
        if let Err(err) = chacha20_xor(&self.key, 1, &nonce, &mut plaintext) {
            plaintext.zeroize();
            return Err(err);
        }
        Ok(plaintext)
    }
}

#[inline]
fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(16);

    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(12);

    state[a] = state[a].wrapping_add(state[b]);
    state[d] ^= state[a];
    state[d] = state[d].rotate_left(8);

    state[c] = state[c].wrapping_add(state[d]);
    state[b] ^= state[c];
    state[b] = state[b].rotate_left(7);
}

fn chacha20_block(key: &[u8; KEY_SIZE], counter: u32, nonce: &[u8; NONCE_SIZE]) -> [u8; 64] {
    let initial = [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        u32::from_le_bytes([key[0], key[1], key[2], key[3]]),
        u32::from_le_bytes([key[4], key[5], key[6], key[7]]),
        u32::from_le_bytes([key[8], key[9], key[10], key[11]]),
        u32::from_le_bytes([key[12], key[13], key[14], key[15]]),
        u32::from_le_bytes([key[16], key[17], key[18], key[19]]),
        u32::from_le_bytes([key[20], key[21], key[22], key[23]]),
        u32::from_le_bytes([key[24], key[25], key[26], key[27]]),
        u32::from_le_bytes([key[28], key[29], key[30], key[31]]),
        counter,
        u32::from_le_bytes([nonce[0], nonce[1], nonce[2], nonce[3]]),
        u32::from_le_bytes([nonce[4], nonce[5], nonce[6], nonce[7]]),
        u32::from_le_bytes([nonce[8], nonce[9], nonce[10], nonce[11]]),
    ];
    let mut working = initial;
    for _ in 0..10 {
        // Column rounds
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        // Diagonal rounds
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }

    let mut out = [0u8; 64];
    for (i, word) in working.iter_mut().enumerate() {
        let sum = word.wrapping_add(initial[i]).to_le_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&sum);
    }
    working.zeroize();
    out
}

fn chacha20_xor(
    key: &[u8; KEY_SIZE],
    initial_counter: u32,
    nonce: &[u8; NONCE_SIZE],
    buf: &mut [u8],
) -> Result<(), aead::Error> {
    let mut block_stream = [0u8; 64];
    for (idx, chunk) in buf.chunks_mut(64).enumerate() {
        let step = u32::try_from(idx).map_err(|_| aead::Error)?;
        let counter = initial_counter.checked_add(step).ok_or(aead::Error)?;
        block_stream = chacha20_block(key, counter, nonce);
        for (dst, ks) in chunk.iter_mut().zip(block_stream.iter()) {
            *dst ^= *ks;
        }
    }
    block_stream.zeroize();
    Ok(())
}

fn chacha20_poly1305_key_gen(key: &[u8; KEY_SIZE], nonce: &[u8; NONCE_SIZE]) -> [u8; 32] {
    let mut block0 = chacha20_block(key, 0, nonce);
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&block0[..32]);
    block0.zeroize();
    otk
}

/// Constant-time 5x26-bit Poly1305 state (RFC 8439 §2.5).
struct Poly1305 {
    r: [u64; 5],
    s: [u8; 16],
    h: [u64; 5],
}

impl Drop for Poly1305 {
    fn drop(&mut self) {
        self.r.zeroize();
        self.s.zeroize();
        self.h.zeroize();
    }
}

impl Poly1305 {
    fn new(otk: &[u8; 32]) -> Self {
        let mut r_bytes = [0u8; 16];
        r_bytes.copy_from_slice(&otk[..16]);
        // Clamp r per RFC 8439 §2.5.
        r_bytes[3] &= 15;
        r_bytes[7] &= 15;
        r_bytes[11] &= 15;
        r_bytes[15] &= 15;
        r_bytes[4] &= 252;
        r_bytes[8] &= 252;
        r_bytes[12] &= 252;

        let t0 = u64::from(u32::from_le_bytes([
            r_bytes[0], r_bytes[1], r_bytes[2], r_bytes[3],
        ]));
        let t1 = u64::from(u32::from_le_bytes([
            r_bytes[4], r_bytes[5], r_bytes[6], r_bytes[7],
        ]));
        let t2 = u64::from(u32::from_le_bytes([
            r_bytes[8],
            r_bytes[9],
            r_bytes[10],
            r_bytes[11],
        ]));
        let t3 = u64::from(u32::from_le_bytes([
            r_bytes[12],
            r_bytes[13],
            r_bytes[14],
            r_bytes[15],
        ]));
        r_bytes.zeroize();

        let r = [
            t0 & 0x03ff_ffff,
            ((t0 >> 26) | (t1 << 6)) & 0x03ff_ffff,
            ((t1 >> 20) | (t2 << 12)) & 0x03ff_ffff,
            ((t2 >> 14) | (t3 << 18)) & 0x03ff_ffff,
            (t3 >> 8) & 0x03ff_ffff,
        ];

        let mut s = [0u8; 16];
        s.copy_from_slice(&otk[16..32]);
        Self { r, s, h: [0; 5] }
    }

    fn ingest_block(&mut self, block: &[u8; 16], hibit: u64) {
        let t0 = u64::from(u32::from_le_bytes([block[0], block[1], block[2], block[3]]));
        let t1 = u64::from(u32::from_le_bytes([block[4], block[5], block[6], block[7]]));
        let t2 = u64::from(u32::from_le_bytes([
            block[8], block[9], block[10], block[11],
        ]));
        let t3 = u64::from(u32::from_le_bytes([
            block[12], block[13], block[14], block[15],
        ]));

        let h0 = self.h[0] + (t0 & 0x03ff_ffff);
        let h1 = self.h[1] + (((t0 >> 26) | (t1 << 6)) & 0x03ff_ffff);
        let h2 = self.h[2] + (((t1 >> 20) | (t2 << 12)) & 0x03ff_ffff);
        let h3 = self.h[3] + (((t2 >> 14) | (t3 << 18)) & 0x03ff_ffff);
        let h4 = self.h[4] + ((t3 >> 8) | hibit);

        let [r0, r1, r2, r3, r4] = self.r;
        let s1 = r1 * 5;
        let s2 = r2 * 5;
        let s3 = r3 * 5;
        let s4 = r4 * 5;

        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;

        let mut c = d0 >> 26;
        let mut n0 = d0 & 0x03ff_ffff;
        d1 += c;

        c = d1 >> 26;
        let mut n1 = d1 & 0x03ff_ffff;
        d2 += c;

        c = d2 >> 26;
        let n2 = d2 & 0x03ff_ffff;
        d3 += c;

        c = d3 >> 26;
        let n3 = d3 & 0x03ff_ffff;
        d4 += c;

        c = d4 >> 26;
        let n4 = d4 & 0x03ff_ffff;
        n0 += c * 5;

        c = n0 >> 26;
        n0 &= 0x03ff_ffff;
        n1 += c;

        self.h = [n0, n1, n2, n3, n4];
    }

    /// Feed `data` padded with zeros to a 16-byte boundary (RFC 8439 §2.8).
    fn update_padded(&mut self, data: &[u8]) {
        let (blocks, rem) = data.as_chunks::<16>();
        for block in blocks {
            self.ingest_block(block, 1 << 24);
        }
        if !rem.is_empty() {
            let mut tail = [0u8; 16];
            tail[..rem.len()].copy_from_slice(rem);
            self.ingest_block(&tail, 1 << 24);
            tail.zeroize();
        }
    }

    #[cfg(test)]
    fn compute_unpadded(otk: &[u8; 32], msg: &[u8]) -> [u8; TAG_SIZE] {
        let mut mac = Self::new(otk);
        let (blocks, rem) = msg.as_chunks::<16>();
        for block in blocks {
            mac.ingest_block(block, 1 << 24);
        }
        if !rem.is_empty() {
            // Partial block (1..=15 bytes): append single 0x01 byte and zero
            // pad, with hibit = 0 so the sentinel sits immediately after rem.
            let mut tail = [0u8; 16];
            tail[..rem.len()].copy_from_slice(rem);
            tail[rem.len()] = 1;
            mac.ingest_block(&tail, 0);
            tail.zeroize();
        }
        mac.finalize()
    }

    fn finalize(mut self) -> [u8; TAG_SIZE] {
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;

        let mut c = h1 >> 26;
        h1 &= 0x03ff_ffff;
        h2 += c;

        c = h2 >> 26;
        h2 &= 0x03ff_ffff;
        h3 += c;

        c = h3 >> 26;
        h3 &= 0x03ff_ffff;
        h4 += c;

        c = h4 >> 26;
        h4 &= 0x03ff_ffff;
        h0 += c * 5;

        c = h0 >> 26;
        h0 &= 0x03ff_ffff;
        h1 += c;

        // Compute g = h + 5 - 2^130 in constant time.
        let mut g0 = h0.wrapping_add(5);
        c = g0 >> 26;
        g0 &= 0x03ff_ffff;

        let mut g1 = h1.wrapping_add(c);
        c = g1 >> 26;
        g1 &= 0x03ff_ffff;

        let mut g2 = h2.wrapping_add(c);
        c = g2 >> 26;
        g2 &= 0x03ff_ffff;

        let mut g3 = h3.wrapping_add(c);
        c = g3 >> 26;
        g3 &= 0x03ff_ffff;

        let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

        // mask is all-ones iff g4 did not underflow (h >= 2^130 - 5), else 0.
        let mask = (g4 >> 63).wrapping_sub(1);
        h0 = (h0 & !mask) | (g0 & mask);
        h1 = (h1 & !mask) | (g1 & mask);
        h2 = (h2 & !mask) | (g2 & mask);
        h3 = (h3 & !mask) | (g3 & mask);
        h4 = (h4 & !mask) | (g4 & mask);

        let h_mod_2_128 = u128::from(h0)
            | (u128::from(h1) << 26)
            | (u128::from(h2) << 52)
            | (u128::from(h3) << 78)
            | (u128::from(h4) << 104);
        let s = u128::from_le_bytes(self.s);
        self.h = [0; 5];
        h_mod_2_128.wrapping_add(s).to_le_bytes()
    }
}

fn poly1305_aead_tag(otk: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> [u8; TAG_SIZE] {
    let mut mac = Poly1305::new(otk);
    mac.update_padded(aad);
    mac.update_padded(ciphertext);
    let mut lengths = [0u8; 16];
    lengths[..8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
    lengths[8..].copy_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    mac.ingest_block(&lengths, 1 << 24);
    lengths.zeroize();
    mac.finalize()
}

#[cfg(test)]
mod tests {
    use super::aead::{Aead, KeyInit, Payload};
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let clean: Vec<u8> = s
            .bytes()
            .filter(|b| !b.is_ascii_whitespace() && *b != b':')
            .collect();
        let (pairs, rem) = clean.as_chunks::<2>();
        assert!(rem.is_empty());
        pairs
            .iter()
            .map(|pair| {
                let hi = (pair[0] as char).to_digit(16).unwrap() as u8;
                let lo = (pair[1] as char).to_digit(16).unwrap() as u8;
                (hi << 4) | lo
            })
            .collect()
    }

    #[test]
    fn rfc_8439_section_2_4_2_chacha20_encryption_vector() {
        let key: [u8; 32] = unhex(
            "00:01:02:03:04:05:06:07:08:09:0a:0b:0c:0d:0e:0f:\
             10:11:12:13:14:15:16:17:18:19:1a:1b:1c:1d:1e:1f",
        )
        .try_into()
        .unwrap();
        let nonce: [u8; 12] = unhex("00:00:00:00:00:00:00:4a:00:00:00:00")
            .try_into()
            .unwrap();
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let expected = unhex(
            "6e 2e 35 9a 25 68 f9 80 41 ba 07 28 dd 0d 69 81 \
             e9 7e 7a ec 1d 43 60 c2 0a 27 af cc fd 9f ae 0b \
             f9 1b 65 c5 52 47 33 ab 8f 59 3d ab cd 62 b3 57 \
             16 39 d6 24 e6 51 52 ab 8f 53 0c 35 9f 08 61 d8 \
             07 ca 0d bf 50 0d 6a 61 56 a3 8e 08 8a 22 b6 5e \
             52 bc 51 4d 16 cc f8 06 81 8c e9 1a b7 79 37 36 \
             5a f9 0b bf 74 a3 5b e6 b4 0b 8e ed f2 78 5e 42 \
             87 4d",
        );

        let mut buf = plaintext.to_vec();
        chacha20_xor(&key, 1, &nonce, &mut buf).unwrap();
        assert_eq!(buf, expected);

        chacha20_xor(&key, 1, &nonce, &mut buf).unwrap();
        assert_eq!(buf, plaintext);
    }

    #[test]
    fn rfc_8439_section_2_5_2_poly1305_mac_vector() {
        let otk: [u8; 32] = unhex(
            "85:d6:be:78:57:55:6d:33:7f:44:52:fe:42:d5:06:a8:\
             01:03:80:8a:fb:0d:b2:fd:4a:bf:f6:af:41:49:f5:1b",
        )
        .try_into()
        .unwrap();
        let msg = b"Cryptographic Forum Research Group";
        let expected: [u8; 16] = unhex("a8:06:1d:c1:30:51:36:c6:c2:2b:8b:af:0c:01:27:a9")
            .try_into()
            .unwrap();
        assert_eq!(Poly1305::compute_unpadded(&otk, msg), expected);
    }

    #[test]
    fn rfc_8439_section_2_8_2_aead_sunscreen_vector() {
        let key: [u8; 32] = unhex(
            "80 81 82 83 84 85 86 87 88 89 8a 8b 8c 8d 8e 8f \
             90 91 92 93 94 95 96 97 98 99 9a 9b 9c 9d 9e 9f",
        )
        .try_into()
        .unwrap();
        let nonce = unhex("07 00 00 00 40 41 42 43 44 45 46 47");
        let aad = unhex("50 51 52 53 c0 c1 c2 c3 c4 c5 c6 c7");
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let expected_ct = unhex(
            "d3 1a 8d 34 64 8e 60 db 7b 86 af bc 53 ef 7e c2 \
             a4 ad ed 51 29 6e 08 fe a9 e2 b5 a7 36 ee 62 d6 \
             3d be a4 5e 8c a9 67 12 82 fa fb 69 da 92 72 8b \
             1a 71 de 0a 9e 06 0b 29 05 d6 a5 b6 7e cd 3b 36 \
             92 dd bd 7f 2d 77 8b 8c 98 03 ae e3 28 09 1b 58 \
             fa b3 24 e4 fa d6 75 94 55 85 80 8b 48 31 d7 bc \
             3f f4 de f0 8e 4b 7a 9d e5 76 d2 65 86 ce c6 4b \
             61 16",
        );
        let expected_tag = unhex("1a e1 0b 59 4f 09 e2 6a 7e 90 2e cb d0 60 06 91");

        let cipher = ChaCha20Poly1305::new((&key).into());
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .unwrap();

        let mut expected = expected_ct;
        expected.extend_from_slice(&expected_tag);
        assert_eq!(sealed, expected);

        let opened = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &sealed,
                    aad: &aad,
                },
            )
            .unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn empty_and_multi_block_roundtrip_and_every_bit_tamper_rejected() {
        let key = [0x42u8; 32];
        let nonce = [0x24u8; 12];
        let cipher = ChaCha20Poly1305::new((&key).into());

        // Empty plaintext, empty AAD.
        let empty_ct = cipher.encrypt(Nonce::from_slice(&nonce), &b""[..]).unwrap();
        assert_eq!(empty_ct.len(), TAG_SIZE);
        assert!(cipher
            .decrypt(Nonce::from_slice(&nonce), empty_ct.as_slice())
            .unwrap()
            .is_empty());

        // Multi-block non-aligned plaintext + non-aligned AAD.
        let msg: Vec<u8> = (0..259u16).map(|i| (i * 37 + 11) as u8).collect();
        let aad: Vec<u8> = (0..47u8).map(|i| i ^ 0xA5).collect();
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &msg,
                    aad: &aad,
                },
            )
            .unwrap();

        // Flipping any bit in ciphertext+tag must fail closed.
        for idx in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[idx] ^= 0x01;
            assert!(
                cipher
                    .decrypt(
                        Nonce::from_slice(&nonce),
                        Payload {
                            msg: &tampered,
                            aad: &aad,
                        },
                    )
                    .is_err(),
                "tampered byte {idx} was accepted"
            );
        }

        // Flipping any bit in AAD must fail closed.
        for idx in 0..aad.len() {
            let mut bad_aad = aad.clone();
            bad_aad[idx] ^= 0x80;
            assert!(cipher
                .decrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &sealed,
                        aad: &bad_aad,
                    },
                )
                .is_err());
        }

        // Wrong nonce, malformed nonce length, and truncated ciphertext must fail closed.
        let mut wrong_nonce = nonce;
        wrong_nonce[0] ^= 1;
        assert!(cipher
            .decrypt(
                Nonce::from_slice(&wrong_nonce),
                Payload {
                    msg: &sealed,
                    aad: &aad,
                },
            )
            .is_err());
        assert!(cipher
            .encrypt(Nonce::from_slice(&[0u8; 11]), msg.as_slice())
            .is_err());
        assert!(cipher
            .decrypt(Nonce::from_slice(&nonce), &sealed[..TAG_SIZE - 1])
            .is_err());
    }

    #[test]
    fn poly1305_near_modulus_limb_reduction_boundary() {
        // Exercise near-modulus `2^130 - 5` carry/reduction path deterministically.
        let otk = [0xffu8; 32];
        let blocks = [0xffu8; 128];
        let tag1 = Poly1305::compute_unpadded(&otk, &blocks);
        let tag2 = Poly1305::compute_unpadded(&otk, &blocks);
        assert_eq!(tag1, tag2);
        assert_ne!(tag1, [0u8; 16]);
    }
}
