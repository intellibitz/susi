//! RFC 7748 X25519 Diffie-Hellman facade backed directly by the workspace's
//! existing `curve25519-dalek` arithmetic (`EdwardsPoint::mul_base_clamped`,
//! `MontgomeryPoint::mul_clamped`, and constant-time `IsIdentity`), eliminating
//! both `x25519-dalek` and `rand_core` from `Cargo.lock` and the four-pillar
//! closure while keeping 100% of elliptic-curve scalar multiplication inside
//! `curve25519-dalek`.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use curve25519_dalek::{edwards::EdwardsPoint, montgomery::MontgomeryPoint, traits::IsIdentity};
use zeroize::Zeroize;

/// RFC 7748 X25519 public key (32-byte compressed Montgomery u-coordinate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PublicKey(MontgomeryPoint);

impl PublicKey {
    /// Convert this public key to a 32-byte array.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Borrow this public key as a 32-byte array.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl From<[u8; 32]> for PublicKey {
    #[inline]
    fn from(bytes: [u8; 32]) -> Self {
        Self(MontgomeryPoint(bytes))
    }
}

impl AsRef<[u8]> for PublicKey {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Reusable / static X25519 secret scalar, zeroized on drop.
///
/// Matches `x25519_dalek::StaticSecret`: stores the 32-byte secret scalar
/// material and applies RFC 7748 scalar clamping inside `mul_base_clamped` and
/// `mul_clamped`.
#[derive(Clone)]
pub struct StaticSecret([u8; 32]);

impl Drop for StaticSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl StaticSecret {
    /// Perform an RFC 7748 X25519 Diffie-Hellman key agreement with
    /// `their_public`.
    #[must_use]
    pub fn diffie_hellman(&self, their_public: &PublicKey) -> SharedSecret {
        SharedSecret(their_public.0.mul_clamped(self.0))
    }

    /// Extract the raw 32-byte secret scalar material.
    #[inline]
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Borrow the raw 32-byte secret scalar material.
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for StaticSecret {
    #[inline]
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for StaticSecret {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl From<&StaticSecret> for PublicKey {
    fn from(secret: &StaticSecret) -> Self {
        Self(EdwardsPoint::mul_base_clamped(secret.0).to_montgomery())
    }
}

/// Result of an X25519 Diffie-Hellman key agreement, zeroized on drop.
pub struct SharedSecret(MontgomeryPoint);

impl Drop for SharedSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SharedSecret {
    /// Convert this shared secret to a 32-byte array.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Borrow this shared secret as a 32-byte array.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Constant-time check that the shared secret is not the all-zero
    /// low-order point (`!is_identity()`).
    #[inline]
    #[must_use]
    pub fn was_contributory(&self) -> bool {
        !self.0.is_identity()
    }
}

impl AsRef<[u8]> for SharedSecret {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_hex_32(hex: &str) -> [u8; 32] {
        let clean: Vec<u8> = hex.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let (pairs, rem) = clean.as_chunks::<2>();
        assert!(rem.is_empty());
        let vec: Vec<u8> = pairs
            .iter()
            .map(|p| {
                let hi = (p[0] as char).to_digit(16).unwrap() as u8;
                let lo = (p[1] as char).to_digit(16).unwrap() as u8;
                (hi << 4) | lo
            })
            .collect();
        vec.try_into().unwrap()
    }

    #[test]
    fn rfc_7748_section_6_1_alice_bob_dh_vector() {
        let alice_priv = StaticSecret::from(decode_hex_32(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ));
        let alice_pub = PublicKey::from(&alice_priv);
        assert_eq!(
            alice_pub.to_bytes(),
            decode_hex_32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );

        let bob_priv = StaticSecret::from(decode_hex_32(
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
        ));
        let bob_pub = PublicKey::from(&bob_priv);
        assert_eq!(
            bob_pub.to_bytes(),
            decode_hex_32("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
        );

        let shared_ab = alice_priv.diffie_hellman(&bob_pub);
        let shared_ba = bob_priv.diffie_hellman(&alice_pub);
        let expected =
            decode_hex_32("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(shared_ab.to_bytes(), expected);
        assert_eq!(shared_ba.to_bytes(), expected);
        assert!(shared_ab.was_contributory());
    }

    #[test]
    fn low_order_all_zero_public_key_is_rejected_as_non_contributory() {
        let secret = StaticSecret::from([0x11; 32]);
        let zero_pub = PublicKey::from([0u8; 32]);
        let shared = secret.diffie_hellman(&zero_pub);
        assert!(!shared.was_contributory());
        assert_eq!(shared.as_bytes(), &[0u8; 32]);
    }
}
