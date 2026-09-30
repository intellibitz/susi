//! Independently verifiable audit evidence exports (VC-201-079).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSegment {
    pub index: u64,
    pub redacted_body: String,
    pub chain_anchor: String,
    pub signature: String,
    pub key_id: String,
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
        let signature = format!("{secret}:{chain_anchor}");
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
            let expected_sig = format!("{secret}:{expected_anchor}");
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
