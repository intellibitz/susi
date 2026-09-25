//! Mutual Cell Authentication via Challenge-Response (Swarm OS Bullet 5)
//!
//! Complements `tls.rs` (transport encryption) and `identity.rs` (per-cell
//! signing keys): before a cell is trusted on an authenticated endpoint, it
//! must sign a caller-issued challenge with the Ed25519 key it registered,
//! proving possession of the private key rather than just knowledge of the
//! public one.

use std::collections::HashMap;
use std::sync::RwLock;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub struct AuthManager {
    trusted: RwLock<HashMap<String, VerifyingKey>>,
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthManager {
    pub fn new() -> Self {
        Self {
            trusted: RwLock::new(HashMap::new()),
        }
    }

    /// Registers `cell_id`'s public key as trusted for future handshakes.
    pub fn trust_cell(&self, cell_id: &str, public_key: &[u8; 32]) -> Result<(), String> {
        let key = VerifyingKey::from_bytes(public_key).map_err(|e| e.to_string())?;
        self.trusted
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cell_id.to_string(), key);
        Ok(())
    }

    /// Verifies that `signature` over `challenge` was produced by
    /// `cell_id`'s registered private key. Unknown cells and bad
    /// signatures both fail closed.
    pub fn verify_cert(&self, cell_id: &str, challenge: &[u8], signature: &[u8; 64]) -> bool {
        let trusted = self.trusted.read().unwrap_or_else(|e| e.into_inner());
        let Some(key) = trusted.get(cell_id) else {
            return false;
        };
        let sig = Signature::from_bytes(signature);
        key.verify(challenge, &sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn genuine_signature_over_the_right_challenge_passes() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let manager = AuthManager::new();
        manager
            .trust_cell("cell-a", signing_key.verifying_key().as_bytes())
            .unwrap();

        let challenge = b"susi-auth-nonce-1";
        let signature = signing_key.sign(challenge);
        assert!(manager.verify_cert("cell-a", challenge, &signature.to_bytes()));
    }

    #[test]
    fn signature_over_the_wrong_challenge_fails() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let manager = AuthManager::new();
        manager
            .trust_cell("cell-a", signing_key.verifying_key().as_bytes())
            .unwrap();

        let signature = signing_key.sign(b"susi-auth-nonce-1");
        assert!(!manager.verify_cert("cell-a", b"different-nonce", &signature.to_bytes()));
    }

    #[test]
    fn unknown_cell_fails_closed() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let manager = AuthManager::new();
        let signature = signing_key.sign(b"susi-auth-nonce-1");
        assert!(!manager.verify_cert("never-trusted", b"susi-auth-nonce-1", &signature.to_bytes()));
    }
}
