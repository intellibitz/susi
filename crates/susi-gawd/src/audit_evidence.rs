//! Independently verifiable audit evidence exports (VC-201-079).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn sign(secret: &str, chain_anchor: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(secret.as_bytes());
    digest.update(b":");
    digest.update(chain_anchor.as_bytes());
    susi_paths::hex::encode(digest.finalize())
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

/// Read an evidence file: an existing export, or a bare JSON array of
/// redacted segments (wrapped in the stable format, its length taken as the
/// expected length).
pub fn read_export(path: &std::path::Path) -> Result<AuditExport, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if let Ok(export) = serde_json::from_slice(&bytes) {
        return Ok(export);
    }
    let segments: Vec<AuditSegment> = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "{} is not a valid audit evidence export: {error}",
            path.display()
        )
    })?;
    Ok(AuditExport {
        format: "susi-audit-evidence/v1".to_string(),
        expected_len: segments.len(),
        segments,
    })
}

/// `susi admin audit-evidence export` / MCP `audit_evidence_export`: wrap
/// `input` in the stable, independently verifiable format and write it to
/// `output`. Returns the number of segments written.
pub fn export_to_file(input: &std::path::Path, output: &std::path::Path) -> Result<usize, String> {
    let export = read_export(input)?;
    let encoded = serde_json::to_vec_pretty(&export)
        .map_err(|error| format!("could not serialize audit evidence: {error}"))?;
    std::fs::write(output, encoded)
        .map_err(|error| format!("could not write {}: {error}", output.display()))?;
    Ok(export.segments.len())
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

#[cfg(test)]
mod export_file_tests {
    use super::*;

    #[test]
    fn export_to_file_wraps_bare_segments_and_round_trips_exports() {
        let dir = std::env::temp_dir().join(format!(
            "susi-audit-evidence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut evidence = AuditEvidence::new();
        evidence.register_key("k1", "test-only-secret");
        evidence.append("first", "k1").unwrap();
        evidence.append("second", "k1").unwrap();
        let bare = dir.join("segments.json");
        std::fs::write(&bare, serde_json::to_vec(&evidence.export()).unwrap()).unwrap();
        let out = dir.join("export.json");
        assert_eq!(export_to_file(&bare, &out).unwrap(), 2);
        let export = read_export(&out).unwrap();
        assert_eq!(export.format, "susi-audit-evidence/v1");
        assert_eq!(export.expected_len, 2);
        let keys = BTreeMap::from([("k1".to_string(), "test-only-secret".to_string())]);
        assert!(export.verify(&keys).is_ok());
        // An existing export re-exports unchanged.
        let again = dir.join("again.json");
        assert_eq!(export_to_file(&out, &again).unwrap(), 2);
        assert_eq!(read_export(&again).unwrap(), export);
        std::fs::write(dir.join("bad.json"), b"not json").unwrap();
        assert!(read_export(&dir.join("bad.json"))
            .unwrap_err()
            .contains("not a valid audit evidence export"));
        assert!(read_export(&dir.join("missing.json"))
            .unwrap_err()
            .contains("could not read"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_audit_evidence_exports_empty_and_verifies_cleanly() {
        let evidence = AuditEvidence::new();
        assert!(evidence.export().is_empty());
        let file_export = evidence.export_file();
        assert_eq!(file_export.format, "susi-audit-evidence/v1");
        assert_eq!(file_export.expected_len, 0);
        let keys = BTreeMap::new();
        assert!(AuditEvidence::verify(&[], &keys).is_ok());
        assert!(file_export.verify(&keys).is_ok());
    }

    #[test]
    fn append_with_unregistered_key_returns_none_without_mutating_chain() {
        let mut evidence = AuditEvidence::new();
        evidence.register_key("k1", "secret-1");
        assert!(evidence.append("unauthenticated", "unknown-key").is_none());
        assert!(evidence.export().is_empty());

        let seg = evidence.append("authenticated", "k1").expect("valid key");
        assert_eq!(seg.index, 0);
        assert_eq!(evidence.export().len(), 1);
    }

    #[test]
    fn verify_detects_tampered_body_signature_and_truncation() {
        let mut evidence = AuditEvidence::new();
        evidence.register_key("k1", "secret-1");
        evidence.append("seg-0", "k1").unwrap();
        evidence.append("seg-1", "k1").unwrap();
        let keys = BTreeMap::from([("k1".to_string(), "secret-1".to_string())]);

        let mut tampered_body = evidence.export();
        tampered_body[1].redacted_body = "forged".into();
        assert_eq!(
            AuditEvidence::verify(&tampered_body, &keys),
            Err(VerifyFailure::Tampering)
        );

        let mut tampered_sig = evidence.export();
        tampered_sig[0].signature = "00".repeat(32);
        assert_eq!(
            AuditEvidence::verify(&tampered_sig, &keys),
            Err(VerifyFailure::Tampering)
        );

        let truncated = &evidence.export()[..1];
        assert_eq!(
            AuditEvidence::verify_against_length(truncated, &keys, 2),
            Err(VerifyFailure::Truncation)
        );
    }

    #[test]
    fn verify_detects_missing_segment_secret_leak_and_wrong_export_format() {
        let mut evidence = AuditEvidence::new();
        evidence.register_key("k1", "super-secret-token");
        evidence.append("seg-0", "k1").unwrap();
        evidence.append("seg-1", "k1").unwrap();
        let keys = BTreeMap::from([("k1".to_string(), "super-secret-token".to_string())]);

        let gap = vec![evidence.export()[1].clone()];
        assert_eq!(
            AuditEvidence::verify(&gap, &keys),
            Err(VerifyFailure::MissingSegment)
        );

        let mut leak_ev = AuditEvidence::new();
        leak_ev.register_key("k1", "super-secret-token");
        leak_ev
            .append("leaked super-secret-token in body", "k1")
            .unwrap();
        assert_eq!(
            AuditEvidence::verify(&leak_ev.export(), &keys),
            Err(VerifyFailure::Tampering)
        );

        let mut bad_format = evidence.export_file();
        bad_format.format = "invalid/v0".to_string();
        assert_eq!(bad_format.verify(&keys), Err(VerifyFailure::Tampering));
    }
}
