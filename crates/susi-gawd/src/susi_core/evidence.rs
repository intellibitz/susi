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
    /// Bound to a live [`crate::susi_core::capture::EvidenceSession`] receipt. Assessment
    /// re-resolves the receipt; a copied hash alone never proves execution.
    ToolReceipt {
        receipt_id: String,
        tool: String,
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
    pub signature: String, // SHA256 integrity checksum, not proof of authorship
}

/// Machine-readable result; missing proof and contradictory proof are distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum EvidenceAssessment {
    Verified,
    Unverified(String),
    Rejected(String),
}

/// Resolve symlinks and reject traversal, absolute escapes, directories and
/// special files before reading. File evidence is always workspace-scoped.
pub(crate) fn confined_file(workspace: &Path, path: &Path) -> Option<PathBuf> {
    let root = workspace.canonicalize().ok()?;
    let target = root.join(path).canonicalize().ok()?;
    (target.starts_with(&root) && target.is_file()).then_some(target)
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
        record.signature = record.calculate_signature().unwrap_or_default();
        record
    }

    fn calculate_signature(&self) -> Option<String> {
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
        .ok()?;
        hasher.update(payload);
        Some(hex::encode(hasher.finalize()))
    }

    /// Capture a file claim at the observation boundary. Verification later
    /// reopens the file, so edits between observation and use invalidate it.
    pub fn capture_file(
        agent_id: &str,
        workspace: &Path,
        path: &Path,
        predicate: &str,
        value: &str,
    ) -> std::io::Result<Self> {
        let target = confined_file(workspace, path).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file outside workspace or not regular",
            )
        })?;
        let content = std::fs::read(target)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs();
        Ok(Self::new(
            agent_id.into(),
            0.0,
            now,
            Claim {
                subject: path.to_string_lossy().into_owned(),
                predicate: predicate.into(),
                value: value.into(),
            },
            EvidenceSource::File {
                path: path.into(),
                hash: hex::encode(Sha256::digest(content)),
            },
            0.0,
        ))
    }

    pub fn render_for_gemi(&self) -> String {
        format!(
            "[UNASSESSED EVIDENCE AGENT: {} (Rank: {:.2}, Confidence: {:.2})]\nClaim: {} {} {}\nSource: {:?}\nSignature: {}\n",
            self.agent_id, self.rank, self.confidence, self.claim.subject, self.claim.predicate, self.claim.value, self.source, self.signature
        )
    }

    /// Integrity is not authenticity: anyone can construct a checksummed record.
    pub fn verify_integrity(&self) -> bool {
        !self.signature.is_empty()
            && self.calculate_signature().as_deref() == Some(self.signature.as_str())
            && self.rank.is_finite()
            && self.rank >= 0.0
            && self.confidence.is_finite()
            && (0.0..=1.0).contains(&self.confidence)
            && !self.agent_id.trim().is_empty()
            && !self.claim.subject.trim().is_empty()
            && !self.claim.predicate.trim().is_empty()
            && !self.claim.value.trim().is_empty()
    }

    /// Re-read the source and validate the actual claim. A checksum or an LLM
    /// observation cannot substitute for this check. No commands are replayed.
    pub fn assess(&self, workspace: &Path) -> EvidenceAssessment {
        use EvidenceAssessment::*;
        if !self.verify_integrity() {
            return Rejected("invalid record integrity or metadata".into());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if self.timestamp == 0 || self.timestamp > now.saturating_add(60) {
            return Rejected("invalid observation timestamp".into());
        }
        match &self.source {
            EvidenceSource::File { path, hash } => {
                let Some(target) = confined_file(workspace, path) else {
                    return Rejected("source is not a regular file inside the workspace".into());
                };
                let Ok(content) = std::fs::read(&target) else {
                    return Rejected("source cannot be read".into());
                };
                if hex::encode(Sha256::digest(&content)) != *hash {
                    return Rejected("source contents changed or hash does not match".into());
                }
                if confined_file(workspace, Path::new(&self.claim.subject)).as_ref()
                    != Some(&target)
                {
                    return Rejected("claim subject does not identify the evidence file".into());
                }
                let supported = match self.claim.predicate.as_str() {
                    "exists" => self.claim.value == "true",
                    "sha256" => self.claim.value == *hash,
                    "equals" => content == self.claim.value.as_bytes(),
                    "contains" => std::str::from_utf8(&content)
                        .is_ok_and(|text| text.contains(&self.claim.value)),
                    _ => return Unverified("unsupported file claim predicate".into()),
                };
                if supported {
                    Verified
                } else {
                    Rejected("file does not support the claim".into())
                }
            }
            EvidenceSource::Command {
                command,
                output_hash,
                ..
            } => {
                if command.trim().is_empty()
                    || output_hash.len() != 64
                    || !output_hash.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Rejected("malformed command evidence".into());
                }
                Unverified("recorded exit code and output hash are not an execution receipt".into())
            }
            EvidenceSource::McpTool {
                tool_name,
                raw_response,
            } => {
                if tool_name.trim().is_empty() || raw_response.trim().is_empty() {
                    return Rejected("empty MCP evidence".into());
                }
                if let Ok(response) = serde_json::from_str::<serde_json::Value>(raw_response) {
                    if response.get("isError").and_then(|v| v.as_bool()) == Some(true)
                        || response.get("error").is_some_and(|v| !v.is_null())
                    {
                        return Rejected("MCP returned an error".into());
                    }
                }
                Unverified("MCP response lacks independently checked claim support".into())
            }
            EvidenceSource::ToolReceipt {
                receipt_id,
                tool,
                output_hash,
            } => {
                if receipt_id.trim().is_empty()
                    || tool.trim().is_empty()
                    || output_hash.len() != 64
                    || !output_hash.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Rejected("malformed tool receipt binding".into());
                }
                let Some(session) = crate::susi_core::capture::EvidenceSession::for_workspace(workspace)
                else {
                    return Unverified("no live evidence session for tool receipt".into());
                };
                let Some(receipt) = session.receipt(receipt_id) else {
                    return Rejected("tool receipt not captured in this mission".into());
                };
                if receipt.tool != *tool || receipt.output_hash != *output_hash {
                    return Rejected("tool receipt binding does not match live ledger".into());
                }
                if !receipt.successful {
                    return Rejected("tool receipt recorded an unsuccessful execution".into());
                }
                // Claim value must appear in the captured output — a bound
                // receipt proves the tool ran, not an arbitrary interpretation.
                let claim_supported = receipt.output.contains(&self.claim.value)
                    || self.claim.predicate == "observed"
                        && self.claim.value == receipt.output_hash;
                if claim_supported {
                    Verified
                } else {
                    Rejected("live receipt does not support the claim value".into())
                }
            }
            EvidenceSource::AgentObservation {
                observation,
                reasoning_trace,
            } => {
                if observation.trim().is_empty() || reasoning_trace.trim().is_empty() {
                    return Rejected("empty agent observation".into());
                }
                Unverified("agent narrative is not independent evidence".into())
            }
            EvidenceSource::System { metric, value } => {
                if metric.trim().is_empty() || value.trim().is_empty() {
                    return Rejected("empty system observation".into());
                }
                Unverified("system metric has no trusted measurement receipt".into())
            }
        }
    }

    pub fn verify_reality(&self, workspace: &Path) -> bool {
        self.assess(workspace) == EvidenceAssessment::Verified
    }
}

