//! Independently verifiable audit evidence exports (VC-201-079).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn sign(secret: &str, chain_anchor: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(secret.as_bytes());
    digest.update(b":");
    digest.update(chain_anchor.as_bytes());
    hex::encode(digest.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSegment {
    pub index: u64,
    pub redacted_body: String,
    pub chain_anchor: String,
    pub signature: String,
    pub key_id: String,
}

/// The file format for independently verifiable audit evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditExport {
    /// Stable format identifier for operators and third-party verifiers.
    pub format: String,
    /// Number of segments in the complete export before any file damage.
    pub expected_len: usize,
    /// Redacted, chained, signed evidence segments.
    pub segments: Vec<AuditSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyFailure {
    Truncation,
    Tampering,
    MissingSegment,
    UnknownKey,
}

#[derive(Debug, Default)]
pub struct AuditEvidence {
    segments: Vec<AuditSegment>,
    keys: BTreeMap<String, String>,
}

impl AuditEvidence {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_key(&mut self, key_id: impl Into<String>, secret: impl Into<String>) {
        self.keys.insert(key_id.into(), secret.into());
    }

    /// Append a redacted segment; signature covers index+body+prev anchor.
    pub fn append(
        &mut self,
        redacted_body: impl Into<String>,
        key_id: &str,
    ) -> Option<AuditSegment> {
        let secret = self.keys.get(key_id)?;
        let index = self.segments.len() as u64;
        let prev = self
            .segments
            .last()
            .map(|s| s.chain_anchor.as_str())
            .unwrap_or("genesis");
        let body = redacted_body.into();
        let chain_anchor = format!("{prev}:{index}:{body}");
        let signature = sign(secret, &chain_anchor);
        let seg = AuditSegment {
            index,
            redacted_body: body,
            chain_anchor,
            signature,
            key_id: key_id.to_string(),
        };
        self.segments.push(seg.clone());
        Some(seg)
    }

    #[must_use]
    pub fn export(&self) -> Vec<AuditSegment> {
        self.segments.clone()
    }

    /// Return a file-ready export with a length commitment so a verifier can
    /// distinguish a truncated tail from a valid shorter chain.
    #[must_use]
    pub fn export_file(&self) -> AuditExport {
        AuditExport {
            format: "susi-audit-evidence/v1".to_string(),
            expected_len: self.segments.len(),
            segments: self.export(),
        }
    }

    /// Verify an exported chain against known keys.
    pub fn verify(
        export: &[AuditSegment],
        keys: &BTreeMap<String, String>,
    ) -> Result<(), VerifyFailure> {
        if export.is_empty() {
            return Ok(());
        }
        // Contiguous indices from 0.
        for (i, seg) in export.iter().enumerate() {
            if seg.index != i as u64 {
                if seg.index > i as u64 {
                    return Err(VerifyFailure::MissingSegment);
                }
                return Err(VerifyFailure::Truncation);
            }
            let Some(secret) = keys.get(&seg.key_id) else {
                return Err(VerifyFailure::UnknownKey);
            };
            let prev = if i == 0 {
                "genesis"
            } else {
                export[i - 1].chain_anchor.as_str()
            };
            let expected_anchor = format!("{prev}:{}:{}", seg.index, seg.redacted_body);
            let expected_sig = sign(secret, &expected_anchor);
            if seg.chain_anchor != expected_anchor || seg.signature != expected_sig {
                return Err(VerifyFailure::Tampering);
            }
            // Secrets must not appear in the redacted body.
            if seg.redacted_body.contains(secret) {
                return Err(VerifyFailure::Tampering);
            }
        }
        Ok(())
    }

    /// Detect truncation: export shorter than known length with gap at end.
    pub fn verify_against_length(
        export: &[AuditSegment],
        keys: &BTreeMap<String, String>,
        expected_len: usize,
    ) -> Result<(), VerifyFailure> {
        if export.len() < expected_len {
            return Err(VerifyFailure::Truncation);
        }
        Self::verify(export, keys)
    }
}

impl AuditExport {
    /// Verify the file envelope and every chained segment against known keys.
    pub fn verify(&self, keys: &BTreeMap<String, String>) -> Result<(), VerifyFailure> {
        if self.format != "susi-audit-evidence/v1" {
            return Err(VerifyFailure::Tampering);
        }
        AuditEvidence::verify_against_length(&self.segments, keys, self.expected_len)
    }
}
