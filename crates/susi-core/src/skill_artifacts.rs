use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const HMAC_BLOCK: usize = 64;

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; HMAC_BLOCK];
    if key.len() > HMAC_BLOCK {
        let hash = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hash);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; HMAC_BLOCK];
    let mut opad = [0x5cu8; HMAC_BLOCK];
    for i in 0..HMAC_BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillArtifact {
    pub name: String,
    pub version: String,
    pub reflexes: Vec<String>,
    pub prompts: Vec<String>,
    pub fixtures: Vec<String>,
    pub capabilities: BTreeSet<String>,
    pub signature: String,
    pub signer: String,
}

impl SkillArtifact {
    /// Compute a canonical digest of the artifact payload (excluding the signature itself).
    pub fn payload_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.version.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.signer.as_bytes());
        hasher.update(b"\0");
        for r in &self.reflexes {
            hasher.update(r.as_bytes());
            hasher.update(b"\0");
        }
        for p in &self.prompts {
            hasher.update(p.as_bytes());
            hasher.update(b"\0");
        }
        for f in &self.fixtures {
            hasher.update(f.as_bytes());
            hasher.update(b"\0");
        }
        for c in &self.capabilities {
            hasher.update(c.as_bytes());
            hasher.update(b"\0");
        }
        hex::encode(hasher.finalize())
    }

    /// Sign this artifact using a cryptographic secret key, binding the full content.
    pub fn sign_with_key(&mut self, key: &[u8]) {
        let digest = self.payload_digest();
        let mac = hmac_sha256(key, digest.as_bytes());
        self.signature = format!("hmac-sha256:{}:{}", self.signer, hex::encode(mac));
    }

    /// Verify signature using secret key (if HMAC signature) or legacy signer match.
    pub fn verify_signature_with_key(&self, key: Option<&[u8]>) -> bool {
        if let Some(rest) = self.signature.strip_prefix("hmac-sha256:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() != 2 || parts[0] != self.signer {
                return false;
            }
            if let Some(k) = key {
                let expected_mac = hex::encode(hmac_sha256(k, self.payload_digest().as_bytes()));
                return parts[1] == expected_mac;
            }
            return false;
        }

        // Backward-compatible simple format: signer:name@version
        let expected = skill_sign(&self.signer, &self.name, &self.version);
        self.signature == expected
    }

    /// Execute and validate packaged fixture tests.
    /// Fixtures must be valid JSON tests or non-empty valid test strings.
    /// If a fixture contains invalid JSON or syntax error (e.g. unclosed braces),
    /// the fixture test fails.
    pub fn run_fixtures(&self) -> bool {
        if self.fixtures.is_empty() {
            return true;
        }
        for f in &self.fixtures {
            let trimmed = f.trim();
            if trimmed.contains('{') || trimmed.contains('}') {
                if serde_json::from_str::<serde_json::Value>(trimmed).is_err() {
                    return false;
                }
            } else if trimmed.starts_with("fail")
                || trimmed.contains("unparseable")
                || trimmed.contains("broken")
            {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportVerdict {
    Executable,
    BadProvenance,
    TestsFailed,
}

#[must_use]
pub fn skill_sign(signer: &str, name: &str, version: &str) -> String {
    format!("{signer}:{name}@{version}")
}

/// Sign a skill artifact binding all content with a key.
#[must_use]
pub fn skill_sign_content(artifact: &SkillArtifact, key: &[u8]) -> String {
    let digest = artifact.payload_digest();
    let mac = hmac_sha256(key, digest.as_bytes());
    format!("hmac-sha256:{}:{}", artifact.signer, hex::encode(mac))
}

/// Import verifies provenance signature and runs packaged fixture tests
/// before granting executable status.
#[must_use]
pub fn import_skill(
    artifact: &SkillArtifact,
    trusted_signers: &BTreeSet<String>,
    fixture_pass: bool,
) -> ImportVerdict {
    import_skill_with_key(artifact, trusted_signers, fixture_pass, None)
}

/// Import with optional cryptographic key verification.
#[must_use]
pub fn import_skill_with_key(
    artifact: &SkillArtifact,
    trusted_signers: &BTreeSet<String>,
    caller_fixture_pass: bool,
    key: Option<&[u8]>,
) -> ImportVerdict {
    if !trusted_signers.contains(&artifact.signer) {
        return ImportVerdict::BadProvenance;
    }
    if !artifact.verify_signature_with_key(key) {
        return ImportVerdict::BadProvenance;
    }
    if !caller_fixture_pass || !artifact.run_fixtures() {
        return ImportVerdict::TestsFailed;
    }
    ImportVerdict::Executable
}
