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
        hex::encode(Sha256::digest(bytes))
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
