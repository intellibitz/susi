//! Swarm Identity & Cryptographic Signatures (Swarm OS Bullet 56)
//!
//! Cells are uniquely identified by an Ed25519 public key. Their actions
//! (syscalls, wire frames) are cryptographically signed to prevent spoofing
//! or Sybil attacks within the decentralized swarm.

use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Represents a Cell's cryptographic identity in the Swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellIdentity {
    pub cell_id: String,
    pub public_key: String, // hex encoded
}

/// Helper struct for managing cell identities and verifying signatures.
pub struct IdentityManager {
    keys: std::collections::HashMap<String, VerifyingKey>,
}

impl IdentityManager {
    pub fn new() -> Self {
        Self {
            keys: std::collections::HashMap::new(),
        }
    }

    /// Generates a new cryptographic identity for a cell.
    pub fn generate_identity(cell_id: &str) -> std::io::Result<(CellIdentity, SigningKey)> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| std::io::Error::other(e.to_string()))?;
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();

        let identity = CellIdentity {
            cell_id: cell_id.to_string(),
            public_key: hex::encode(verifying_key.as_bytes()),
        };

        Ok((identity, signing_key))
    }

    /// Registers a cell's public key with the Swarm OS.
    pub fn register_identity(&mut self, identity: &CellIdentity) -> Result<(), String> {
        let bytes = hex::decode(&identity.public_key).map_err(|e| e.to_string())?;

        let mut key_bytes = [0u8; 32];
        if bytes.len() != 32 {
            return Err("Invalid Ed25519 public key length".into());
        }
        key_bytes.copy_from_slice(&bytes);

        let verifying_key = VerifyingKey::from_bytes(&key_bytes).map_err(|e| e.to_string())?;
        self.keys.insert(identity.cell_id.clone(), verifying_key);
        Ok(())
    }

    /// Verifies the cryptographic signature of a payload.
    pub fn verify_signature(&self, cell_id: &str, payload: &[u8], signature_hex: &str) -> bool {
        if let Some(vk) = self.keys.get(cell_id)
            && let Ok(sig_bytes) = hex::decode(signature_hex)
            && let Ok(signature) = Signature::from_slice(&sig_bytes)
        {
            return vk.verify(payload, &signature).is_ok();
        }
        false
    }
}

impl Default for IdentityManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn test_identity_generation_and_verification() {
        let mut manager = IdentityManager::new();

        // 1. Generate identity
        let (identity, signing_key) = IdentityManager::generate_identity("test-cell").unwrap();

        // 2. Register identity
        manager.register_identity(&identity).unwrap();

        // 3. Sign a payload
        let payload = b"hello swarm";
        let signature = signing_key.sign(payload);
        let sig_hex = hex::encode(signature.to_bytes());

        // 4. Verify signature
        assert!(manager.verify_signature("test-cell", payload, &sig_hex));

        // 5. Spoofed payload should fail
        assert!(!manager.verify_signature("test-cell", b"spoofed swarm", &sig_hex));

        // 6. Unknown cell should fail
        assert!(!manager.verify_signature("unknown-cell", payload, &sig_hex));
    }
}
