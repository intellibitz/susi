//! Evaluation receipts bound to candidate artifacts (VC-201-004).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalReceipt {
    pub source_revision: String,
    pub artifact_digest: String,
    pub suite_revision: String,
    pub environment: String,
    pub exit_status: i32,
}

impl EvalReceipt {
    #[must_use]
    pub fn digest_bytes(bytes: &[u8]) -> String {
        susi_paths::hex::encode(Sha256::digest(bytes))
    }

    #[must_use]
    pub fn new(
        source_revision: impl Into<String>,
        artifact: &[u8],
        suite_revision: impl Into<String>,
        environment: impl Into<String>,
        exit_status: i32,
    ) -> Self {
        Self {
            source_revision: source_revision.into(),
            artifact_digest: Self::digest_bytes(artifact),
            suite_revision: suite_revision.into(),
            environment: environment.into(),
            exit_status,
        }
    }

    /// A changed artifact or missing receipt invalidates the comparison.
    #[must_use]
    pub fn validates_against(&self, artifact: &[u8], other: Option<&Self>) -> bool {
        let Some(other) = other else {
            return false;
        };
        self.artifact_digest == Self::digest_bytes(artifact)
            && self.artifact_digest == other.artifact_digest
            && self.suite_revision == other.suite_revision
            && self.source_revision == other.source_revision
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn digest_bytes_produces_deterministic_64_char_sha256_hex() {
        let d1 = EvalReceipt::digest_bytes(b"candidate-binary-v1");
        let d2 = EvalReceipt::digest_bytes(b"candidate-binary-v1");
        let d3 = EvalReceipt::digest_bytes(b"candidate-binary-v2");
        assert_eq!(d1.len(), 64);
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
    }

    #[test]
    fn new_populates_fields_and_computes_artifact_digest() {
        let artifact = b"wasm-payload";
        let r = EvalReceipt::new("git-abc", artifact, "suite-1", "linux-x86_64", 0);
        assert_eq!(r.source_revision, "git-abc");
        assert_eq!(r.artifact_digest, EvalReceipt::digest_bytes(artifact));
        assert_eq!(r.suite_revision, "suite-1");
        assert_eq!(r.environment, "linux-x86_64");
        assert_eq!(r.exit_status, 0);
    }

    #[test]
    fn validates_against_accepts_matching_receipt_and_artifact() {
        let artifact = b"verified-artifact";
        let r1 = EvalReceipt::new("rev-1", artifact, "suite-2", "env-a", 0);
        let r2 = EvalReceipt::new("rev-1", artifact, "suite-2", "env-b", 0);
        assert!(r1.validates_against(artifact, Some(&r2)));
    }

    #[test]
    fn validates_against_rejects_none_tampered_artifact_or_mismatched_revisions() {
        let artifact = b"verified-artifact";
        let base = EvalReceipt::new("rev-1", artifact, "suite-2", "env-a", 0);
        assert!(!base.validates_against(artifact, None));
        assert!(!base.validates_against(b"tampered", Some(&base)));

        let diff_suite = EvalReceipt::new("rev-1", artifact, "suite-3", "env-a", 0);
        assert!(!base.validates_against(artifact, Some(&diff_suite)));

        let diff_source = EvalReceipt::new("rev-2", artifact, "suite-2", "env-a", 0);
        assert!(!base.validates_against(artifact, Some(&diff_source)));
    }

    #[test]
    fn eval_receipt_serde_json_roundtrip_preserves_all_fields() {
        let receipt = EvalReceipt::new("rev-10", b"payload-10", "suite-9", "windows-x86_64", 1);
        let encoded = serde_json::to_string(&receipt).unwrap();
        let decoded: EvalReceipt = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, receipt);
    }
}
