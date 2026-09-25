//! E2E Payload Encryption for IPC (Swarm OS Bullet 42)
//!
//! Symmetric AEAD sealing for cell-to-cell payloads, the same
//! ChaCha20-Poly1305 scheme `susi_config::member_seal` uses for pairwise
//! peer channels, but keyed per `Encryptor` instance rather than derived
//! from a node's cluster identity.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

use crate::susi_error::EaiError;

pub struct Encryptor {
    cipher: ChaCha20Poly1305,
}

impl Encryptor {
    /// Builds an encryptor bound to a 256-bit key.
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
        }
    }

    /// Generates a fresh random 256-bit key.
    pub fn generate_key() -> Result<[u8; 32], EaiError> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| EaiError::internal(e.to_string()))?;
        Ok(key)
    }

    /// Seals `payload`, returning `(nonce, ciphertext)`. The nonce must
    /// accompany the ciphertext to `decrypt` it.
    pub fn encrypt(&self, payload: &[u8]) -> Result<([u8; 12], Vec<u8>), EaiError> {
        let mut nonce_bytes = [0u8; 12];
        getrandom::fill(&mut nonce_bytes).map_err(|e| EaiError::internal(e.to_string()))?;
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), payload)
            .map_err(|_| EaiError::internal("encryption failed"))?;
        Ok((nonce_bytes, ciphertext))
    }

    /// Opens a ciphertext produced by `encrypt`. Fails closed on a
    /// tampered tag or wrong key — there is no partial plaintext.
    pub fn decrypt(&self, nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Vec<u8>, EaiError> {
        self.cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| EaiError::internal("decryption failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_payload() {
        let key = Encryptor::generate_key().unwrap();
        let enc = Encryptor::new(&key);
        let (nonce, ciphertext) = enc.encrypt(b"swarm secret").unwrap();
        assert_ne!(ciphertext, b"swarm secret");
        assert_eq!(enc.decrypt(&nonce, &ciphertext).unwrap(), b"swarm secret");
    }

    #[test]
    fn wrong_key_fails_closed() {
        let enc_a = Encryptor::new(&Encryptor::generate_key().unwrap());
        let enc_b = Encryptor::new(&Encryptor::generate_key().unwrap());
        let (nonce, ciphertext) = enc_a.encrypt(b"payload").unwrap();
        assert!(enc_b.decrypt(&nonce, &ciphertext).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_closed() {
        let key = Encryptor::generate_key().unwrap();
        let enc = Encryptor::new(&key);
        let (nonce, mut ciphertext) = enc.encrypt(b"payload").unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xFF;
        assert!(enc.decrypt(&nonce, &ciphertext).is_err());
    }
}
