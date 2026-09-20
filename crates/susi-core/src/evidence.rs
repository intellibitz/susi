// Structured record type for an agent's claim plus the evidence backing it
// (a file hash, a command's exit code, etc.), so the claim can be checked
// against the workspace later.

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
    McpTool {
        tool_name: String,
        raw_response: String,
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
    pub signature: String, // SHA256 integrity checksum, not proof of authorship
}

impl EvidenceRecord {
    #[allow(clippy::too_many_arguments)]
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
        // Serialize a tuple to preserve field boundaries and bind every field.
        let payload = serde_json::to_vec(&(
            &self.agent_id,
            self.rank.to_bits(),
            self.timestamp,
            &self.claim,
            &self.source,
            self.confidence.to_bits(),
        ))
        .expect("evidence fields are serializable");
        hasher.update(payload);
        hex::encode(hasher.finalize())
    }

    pub fn render_for_gemi(&self) -> String {
        format!(
            "[EVIDENCE AGENT: {} (Rank: {:.2}, Confidence: {:.2})]\nClaim: {} {} {}\nSource: {:?}\nSignature: {}\n",
            self.agent_id, self.rank, self.confidence, self.claim.subject, self.claim.predicate, self.claim.value, self.source, self.signature
        )
    }

    pub fn verify_reality(&self, workspace: &Path) -> bool {
        // 1. The record hasn't been tampered with since it was created.
        if self.signature != self.calculate_signature()
            || !self.rank.is_finite()
            || !self.confidence.is_finite()
            || !(0.0..=1.0).contains(&self.confidence)
            || self.agent_id.trim().is_empty()
            || self.claim.subject.trim().is_empty()
            || self.claim.predicate.trim().is_empty()
            || self.claim.value.trim().is_empty()
        {
            return false;
        }

        // 2. The claimed evidence still checks out physically.
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
            EvidenceSource::McpTool { raw_response, .. } => {
                // An empty claim or a recorded error from an MCP tool is physically invalid
                if raw_response.trim().is_empty() || raw_response.to_lowercase().contains("error") {
                    return false;
                }
                true
            }
            EvidenceSource::AgentObservation {
                observation,
                reasoning_trace,
            } => !observation.trim().is_empty() && !reasoning_trace.trim().is_empty(),
            EvidenceSource::System { metric, value } => {
                !metric.trim().is_empty() && !value.trim().is_empty()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> EvidenceRecord {
        EvidenceRecord::new(
            "agent".into(),
            0.5,
            1,
            Claim {
                subject: "a".into(),
                predicate: "bc".into(),
                value: "d".into(),
            },
            EvidenceSource::AgentObservation {
                observation: "observed".into(),
                reasoning_trace: "trace".into(),
            },
            0.5,
        )
    }

    #[test]
    fn integrity_binds_rank_confidence_and_field_boundaries() {
        let original = observation();
        assert!(original.verify_reality(Path::new(".")));
        let mut changed = original.clone();
        changed.rank = 1.0;
        assert!(!changed.verify_reality(Path::new(".")));
        let mut changed = original.clone();
        changed.confidence = 1.0;
        assert!(!changed.verify_reality(Path::new(".")));
        let mut changed = original.clone();
        changed.claim.subject = "ab".into();
        changed.claim.predicate = "c".into();
        assert!(!changed.verify_reality(Path::new(".")));
        assert!(!original.render_for_gemi().contains("VERIFIED"));
        let restored: EvidenceRecord =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert!(restored.verify_reality(Path::new(".")));
    }

    #[test]
    fn empty_observations_and_invalid_confidence_are_rejected() {
        let mut record = observation();
        record.source = EvidenceSource::AgentObservation {
            observation: " ".into(),
            reasoning_trace: "trace".into(),
        };
        record.signature = record.calculate_signature();
        assert!(!record.verify_reality(Path::new(".")));
        let mut record = observation();
        record.confidence = f32::NAN;
        record.signature = record.calculate_signature();
        assert!(!record.verify_reality(Path::new(".")));
    }
}
