//! Generic exchange identifiers; never used as a secret key.
use sha2::{Digest, Sha256};

/// Fresh random nonce for one handshake exchange (128-bit).
pub fn random_nonce_hex() -> String {
    let mut raw = [0u8; 16];
    if getrandom::fill(&mut raw).is_err() {
        // Fallback: hash pid + nanos — still per-exchange unique enough to
        // bind a pong to this ping (this is a nonce, not a secret).
        let seed = format!(
            "{}:{}",
            std::process::id(),
            susi_paths::unix_epoch_elapsed().as_nanos()
        );
        let digest = Sha256::digest(seed.as_bytes());
        return susi_paths::hex::encode(&digest[..16]);
    }
    susi_paths::hex::encode(raw)
}
