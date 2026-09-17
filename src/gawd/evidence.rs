// SUSI Evidence Intermediate Representation (Evidence IR)
// Architecture Refinement: Structured Provenance & Claim Verification

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceSource {
    File {
        path: PathBuf,
        hash: String,
    },
    Command {
        command: String,
        exit_code: i32,
        output_hash: String,
    },
    System {
        metric: String,
        value: String,
    },
    AgentObservation {
        observation: String,
        reasoning_trace: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub subject: String,
    pub predicate: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub agent_id: String,
    pub rank: f32,
    pub timestamp: u64,
    pub claim: Claim,
    pub source: EvidenceSource,
    pub confidence: f32,
    pub signature: String, // SHA256 cryptographic link of the entire record
}

impl EvidenceRecord {
    pub fn new(
        agent_id: String,
        rank: f32,
        timestamp: u64,
        claim: Claim,
        source: EvidenceSource,
        confidence: f32,
    ) -> Self {
        let mut record = EvidenceRecord {
            agent_id,
            rank,
            timestamp,
            claim,
            source,
            confidence,
            signature: String::new(),
        };
        record.signature = record.calculate_signature();
        record
    }

    fn calculate_signature(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.agent_id.as_bytes());
        hasher.update(self.timestamp.to_le_bytes());
        hasher.update(self.claim.subject.as_bytes());
        hasher.update(self.claim.predicate.as_bytes());
        hasher.update(self.claim.value.as_bytes());
        hasher.update(format!("{:?}", self.source).as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn render_for_gemi(&self) -> String {
        format!(
            "[VERIFIED AGENT: {} (Rank: {:.2}, Confidence: {:.2})]\nClaim: {} {} {}\nSource: {:?}\nSignature: {}\n",
            self.agent_id, self.rank, self.confidence, self.claim.subject, self.claim.predicate, self.claim.value, self.source, self.signature
        )
    }

    pub fn verify_reality(&self, workspace: &Path) -> bool {
        // 1. Signature Check (Substrate Integrity)
        if self.signature != self.calculate_signature() {
            return false;
        }

        // 2. Physical Truth Check
        match &self.source {
            EvidenceSource::File { path, hash } => {
                let target = workspace.join(path);
                if !target.exists() {
                    return false;
                }
                if let Ok(content) = std::fs::read(target) {
                    let mut hasher = Sha256::new();
                    hasher.update(content);
                    let actual_hash = hex::encode(hasher.finalize());
                    return actual_hash == *hash;
                }
                false
            }
            EvidenceSource::Command { exit_code, .. } => *exit_code == 0,
            _ => true,
        }
    }
}
