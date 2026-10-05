//! Structured audit export for SIEM.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Maximum size of any human-readable audit metadata field.
pub const MAX_AUDIT_METADATA_CHARS: usize = 240;

/// Exhaustive vocabulary for state-changing actions.  Payloads are not an
/// action kind: callers attach only a digest and byte count through
/// [`AuditTarget::payload_ref`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActionKind {
    TaskTransition,
    ClaimTransition,
    Commit,
    Push,
    Merge,
    ModelCall,
    ToolExecution,
    FileWrite,
    EgressAttempt,
    Approval,
    SecretAccess,
    MissionAdmission,
    MissionTransition,
    LeaseRelease,
    EvidenceRecord,
    ConfigurationChange,
    Deployment,
}

impl AuditActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskTransition => "task_transition",
            Self::ClaimTransition => "claim_transition",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::Merge => "merge",
            Self::ModelCall => "model_call",
            Self::ToolExecution => "tool_execution",
            Self::FileWrite => "file_write",
            Self::EgressAttempt => "egress_attempt",
            Self::Approval => "approval",
            Self::SecretAccess => "secret_access",
            Self::MissionAdmission => "mission_admission",
            Self::MissionTransition => "mission_transition",
            Self::LeaseRelease => "lease_release",
            Self::EvidenceRecord => "evidence_record",
            Self::ConfigurationChange => "configuration_change",
            Self::Deployment => "deployment",
        }
    }
}

/// Outcome of one dispatched state-changing action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Succeeded,
    Denied,
    Failed,
    Cancelled,
    Partial,
    Uncertain,
}

impl AuditOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Partial => "partial",
            Self::Uncertain => "uncertain",
        }
    }
}

/// A reference to content that must not enter the durable audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadReference {
    pub sha256: String,
    pub bytes: u64,
    pub hint: String,
}

impl PayloadReference {
    #[must_use]
    pub fn from_bytes(payload: &[u8], hint: &str) -> Self {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(payload);
        Self {
            sha256: hex::encode(hasher.finalize()),
            bytes: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            hint: bounded_metadata(hint, 64),
        }
    }
}

/// Target metadata for an action.  The optional payload reference proves
/// which content was involved without retaining what was said or read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditTarget {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<PayloadReference>,
}

impl AuditTarget {
    #[must_use]
    pub fn identifier(id: &str) -> Self {
        Self {
            id: bounded_metadata(id, MAX_AUDIT_METADATA_CHARS),
            payload: None,
        }
    }

    #[must_use]
    pub fn payload_ref(id: &str, payload: &[u8], hint: &str) -> Self {
        Self {
            id: bounded_metadata(id, MAX_AUDIT_METADATA_CHARS),
            payload: Some(PayloadReference::from_bytes(payload, hint)),
        }
    }
}

/// The only record shape accepted by the production audit choke point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAction {
    pub action_kind: AuditActionKind,
    pub actor: String,
    pub target: AuditTarget,
    pub outcome: AuditOutcome,
    pub duration_ms: u64,
    pub cost_micros: u64,
    pub correlation_id: String,
    /// Always true for records constructed by this module.  Keeping the
    /// marker in the record makes redaction visible to evidence consumers.
    pub redacted: bool,
}

impl AuditAction {
    /// Build an action whose fields are metadata only.
    #[allow(clippy::too_many_arguments)] // the constructor mirrors the fixed evidence schema so callers cannot omit cost, timing, or correlation
    pub fn new(
        action_kind: AuditActionKind,
        actor: &str,
        target: AuditTarget,
        outcome: AuditOutcome,
        duration_ms: u64,
        cost_micros: u64,
        correlation_id: &str,
    ) -> susi_error::EaiResult<Self> {
        let actor = bounded_metadata(actor, MAX_AUDIT_METADATA_CHARS);
        let correlation_id = bounded_metadata(correlation_id, 128);
        if actor.is_empty() {
            return Err(susi_error::EaiError::governance(
                "audit action requires an actor",
            ));
        }
        if correlation_id.is_empty() {
            return Err(susi_error::EaiError::governance(
                "audit action requires a correlation id",
            ));
        }
        if target.id.is_empty() {
            return Err(susi_error::EaiError::governance(
                "audit action requires a target",
            ));
        }
        Ok(Self {
            action_kind,
            actor,
            target,
            outcome,
            duration_ms,
            cost_micros,
            correlation_id,
            redacted: true,
        })
    }

    /// Convert a legacy tool/detail pair into a payload-free action.  The
    /// detail is represented by a digest and byte count, never copied into
    /// the audit record.
    pub fn from_legacy(
        tool: &str,
        detail: &str,
        outcome: AuditOutcome,
    ) -> susi_error::EaiResult<Self> {
        let target = AuditTarget::payload_ref(tool, detail.as_bytes(), "action_detail");
        let correlation_id = format!(
            "{}:{}",
            tool,
            target.payload.as_ref().map_or("", |p| &p.sha256)
        );
        Self::new(
            AuditActionKind::ToolExecution,
            "runtime",
            target,
            outcome,
            0,
            0,
            &correlation_id,
        )
    }

    fn validate(&self) -> susi_error::EaiResult<()> {
        if !self.redacted {
            return Err(susi_error::EaiError::governance(
                "unredacted action cannot enter the audit choke point",
            ));
        }
        if self.actor.is_empty() || self.target.id.is_empty() || self.correlation_id.is_empty() {
            return Err(susi_error::EaiError::governance(
                "audit action metadata is incomplete",
            ));
        }
        Ok(())
    }
}

/// The single sink contract used by the production audit dispatcher.
pub trait AuditSink: Send + Sync {
    fn append(&self, action: &AuditAction) -> susi_error::EaiResult<()>;
}

/// Single dispatch point for structured state-change records.
pub struct AuditChokePoint<S> {
    sink: S,
}

impl<S: AuditSink> AuditChokePoint<S> {
    #[must_use]
    pub const fn new(sink: S) -> Self {
        Self { sink }
    }

    /// Validate and append exactly one record to the configured sink.
    pub fn dispatch(&self, action: &AuditAction) -> susi_error::EaiResult<()> {
        action.validate()?;
        self.sink.append(action)
    }
}

/// In-memory sink for deterministic integration tests and host adapters.
#[derive(Clone, Default)]
pub struct RecordingAuditSink {
    records: Arc<Mutex<Vec<AuditAction>>>,
}

impl RecordingAuditSink {
    #[must_use]
    pub fn records(&self) -> Vec<AuditAction> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl AuditSink for RecordingAuditSink {
    fn append(&self, action: &AuditAction) -> susi_error::EaiResult<()> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(action.clone());
        Ok(())
    }
}

fn bounded_metadata(value: &str, max_chars: usize) -> String {
    let redacted = susi_config::redact_credentials(value);
    redacted.chars().take(max_chars).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub ts_unix: u64,
    pub action: String,
    pub actor: String,
    pub outcome: String,
}

/// Export events as newline-delimited JSON for SIEM ingest.
#[must_use]
pub fn export_ndjson(events: &[AuditEvent]) -> String {
    events
        .iter()
        .map(|e| {
            json!({
                "ts": e.ts_unix,
                "action": e.action,
                "actor": e.actor,
                "outcome": e.outcome,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse one NDJSON line back (round-trip helper).
pub fn parse_ndjson_line(line: &str) -> Result<Value, String> {
    serde_json::from_str(line).map_err(|e| e.to_string())
}

#[cfg(test)]
mod audit_export_tests {
    use super::*;

    #[test]
    fn audit_export_ndjson_for_siem() {
        let nd = export_ndjson(&[AuditEvent {
            ts_unix: 1,
            action: "start".into(),
            actor: "daemon".into(),
            outcome: "ok".into(),
        }]);
        let v = parse_ndjson_line(&nd).unwrap();
        assert_eq!(v["action"], "start");
        assert_eq!(v["outcome"], "ok");
    }
}
