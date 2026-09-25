//! Cell Binary Signing & Verification (Swarm OS Bullet 51)
//!
//! Deny-by-default gate: a WASM cell binary is only accepted if it carries
//! a valid Ed25519 signature from a publisher key this policy explicitly
//! trusts. Complements `hot_reload.rs` (which validates that accepted
//! bytes are a loadable module) and `identity.rs` (per-cell signing keys)
//! with the publisher-trust half of "cell binaries are signed and
//! verified before execution; unsigned code is rejected by default."

use std::collections::HashSet;
use std::sync::RwLock;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub struct CodeSigningPolicy {
    trusted_publishers: RwLock<HashSet<[u8; 32]>>,
}

impl Default for CodeSigningPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeSigningPolicy {
    pub fn new() -> Self {
        Self {
            trusted_publishers: RwLock::new(HashSet::new()),
        }
    }

    /// Adds `publisher_key` to the trusted-publisher set.
    pub fn trust_publisher(&self, publisher_key: [u8; 32]) {
        self.trusted_publishers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(publisher_key);
    }

    /// Verifies `binary`'s detached signature against a trusted publisher
    /// key. Deny-by-default: an empty trust set, an unrecognized key, and
    /// a bad signature are all rejected identically.
    pub fn verify(
        &self,
        binary: &[u8],
        publisher_key: &[u8; 32],
        signature: &[u8; 64],
    ) -> Result<(), String> {
        let trusted = self
            .trusted_publishers
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if !trusted.contains(publisher_key) {
            return Err("publisher key is not trusted".to_string());
        }
        let key = VerifyingKey::from_bytes(publisher_key).map_err(|e| e.to_string())?;
        let sig = Signature::from_bytes(signature);
        key.verify(binary, &sig)
            .map_err(|_| "signature verification failed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn trusted_publisher_valid_signature_passes() {
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let policy = CodeSigningPolicy::new();
        policy.trust_publisher(*signing_key.verifying_key().as_bytes());

        let binary = b"wasm-bytes-here";
        let sig = signing_key.sign(binary);
        assert!(
            policy
                .verify(
                    binary,
                    signing_key.verifying_key().as_bytes(),
                    &sig.to_bytes()
                )
                .is_ok()
        );
    }

    #[test]
    fn untrusted_publisher_is_rejected_even_with_a_valid_signature() {
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let policy = CodeSigningPolicy::new(); // publisher never trusted
        let binary = b"wasm-bytes-here";
        let sig = signing_key.sign(binary);
        assert!(
            policy
                .verify(
                    binary,
                    signing_key.verifying_key().as_bytes(),
                    &sig.to_bytes()
                )
                .is_err()
        );
    }

    #[test]
    fn tampered_binary_fails_verification() {
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let policy = CodeSigningPolicy::new();
        policy.trust_publisher(*signing_key.verifying_key().as_bytes());

        let sig = signing_key.sign(b"original");
        assert!(
            policy
                .verify(
                    b"tampered",
                    signing_key.verifying_key().as_bytes(),
                    &sig.to_bytes()
                )
                .is_err()
        );
    }
}
