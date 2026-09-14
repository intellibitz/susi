// SUSI Evidence Intermediate Representation (Evidence IR)
// Architecture Refinement: Structured Provenance & Claim Verification

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceSource {
    File { path: PathBuf, hash: String },
    Command { command: String },
    System { metric: String },
    AgentObservation { observation: String },
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
}

impl EvidenceRecord {
    pub fn render_for_gemi(&self) -> String {
        format!(
            "[AGENT: {} (Rank: {:.2}, Confidence: {:.2})]\nClaim: {} {} {}\nSource: {:?}\n",
            self.agent_id, self.rank, self.confidence, self.claim.subject, self.claim.predicate, self.claim.value, self.source
        )
    }

    pub fn verify_reality(&self, workspace: &std::path::Path) -> bool {
        match &self.source {
            EvidenceSource::File { path, .. } => {
                let target = workspace.join(path);
                target.exists()
            }
            _ => true, // Non-file evidence defaults to valid
        }
    }
}
