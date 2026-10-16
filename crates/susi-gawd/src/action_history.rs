//! Tamper-evident and interrogable action history (VC-202-019).
//!
//! Each action record is chained to its predecessor under an existing HMAC audit key,
//! verified with length commitments, and periodic head anchors are captured so the
//! history is immutable evidence rather than a mutable file.

use crate::audit_evidence::{AuditEvidence, AuditExport, AuditSegment, VerifyFailure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An individual action record within the chained action history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    /// Unique identifier for the action.
    pub id: String,
    /// Originating agent identifier.
    pub agent_id: String,
    /// Associated mission identifier.
    pub mission_id: String,
    /// Action category or type (e.g., tool_call, git_commit).
    pub kind: String,
    /// Unix timestamp when the action occurred.
    pub timestamp_unix: u64,
    /// Monetary or execution cost in micro-units.
    pub cost_micros: u64,
    /// Descriptive payload or parameters of the action.
    pub payload: String,
    /// The signed and chained evidence segment.
    pub segment: AuditSegment,
}

/// A periodic snapshot of the chain head anchor and signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainAnchor {
    /// Segment index at which the anchor was taken.
    pub anchor_index: u64,
    /// The chain anchor digest at this point in history.
    pub head_anchor: String,
    /// The cryptographic signature over the head anchor.
    pub head_signature: String,
    /// Unix timestamp when the anchor was captured.
    pub anchored_unix: u64,
}

/// Chained, tamper-evident action history with multi-dimensional interrogation surfaces.
#[derive(Debug, Default)]
pub struct ActionHistory {
    /// Underlying cryptographic audit evidence log.
    pub evidence: AuditEvidence,
    /// Chronological action records.
    pub records: Vec<ActionRecord>,
    /// Periodic head anchor checkpoints.
    pub anchors: Vec<ChainAnchor>,
    /// Anchor frequency interval.
    pub anchor_interval: usize,
    /// Identifier of the active signing key.
    pub signing_key_id: String,
}

impl ActionHistory {
    /// Create a new action history with a designated signing key and periodic anchor interval.
    #[must_use]
    pub fn new(signing_key_id: &str, secret: &str, anchor_interval: usize) -> Self {
        let mut evidence = AuditEvidence::new();
        evidence.register_key(signing_key_id, secret);
        Self {
            evidence,
            records: Vec::new(),
            anchors: Vec::new(),
            anchor_interval: anchor_interval.max(1),
            signing_key_id: signing_key_id.to_string(),
        }
    }

    /// Record an action, append a chained evidence segment, and record an anchor if due.
    #[allow(clippy::too_many_arguments)] // Action logging records agent, mission, kind, timestamp, cost, and payload atomically.
    pub fn record_action(
        &mut self,
        agent_id: &str,
        mission_id: &str,
        kind: &str,
        timestamp_unix: u64,
        cost_micros: u64,
        payload: &str,
    ) -> Option<&ActionRecord> {
        let action_id = format!("act-{}-{}", self.records.len(), timestamp_unix);
        let canonical_body = format!(
            "{}:{}:{}:{}:{}:{}",
            action_id, agent_id, mission_id, kind, timestamp_unix, payload
        );

        let segment = self
            .evidence
            .append(&canonical_body, &self.signing_key_id)?;

        let rec = ActionRecord {
            id: action_id,
            agent_id: agent_id.to_string(),
            mission_id: mission_id.to_string(),
            kind: kind.to_string(),
            timestamp_unix,
            cost_micros,
            payload: payload.to_string(),
            segment: segment.clone(),
        };
        self.records.push(rec);

        // Periodically anchor the chain head
        if self.records.len().is_multiple_of(self.anchor_interval) {
            self.anchors.push(ChainAnchor {
                anchor_index: segment.index,
                head_anchor: segment.chain_anchor,
                head_signature: segment.signature,
                anchored_unix: timestamp_unix,
            });
        }

        self.records.last()
    }

    /// Export the underlying audit evidence file with format and length commitments.
    #[must_use]
    pub fn export_file(&self) -> AuditExport {
        self.evidence.export_file()
    }

    /// Access all recorded actions.
    #[must_use]
    pub fn records(&self) -> &[ActionRecord] {
        &self.records
    }

    /// Access all periodic chain anchors.
    #[must_use]
    pub fn anchors(&self) -> &[ChainAnchor] {
        &self.anchors
    }

    /// Access the underlying audit evidence.
    #[must_use]
    pub fn evidence(&self) -> &AuditEvidence {
        &self.evidence
    }

    /// Returns the number of recorded actions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true if no actions have been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Verify full chain integrity and historical anchor validity against provided keys.
    pub fn verify(&self, keys: &BTreeMap<String, String>) -> Result<(), VerifyFailure> {
        let segments = self.evidence.export();
        AuditEvidence::verify_against_length(&segments, keys, self.records.len())?;

        // Verify periodic anchor points match historical record chain states
        for anchor in &self.anchors {
            let idx = anchor.anchor_index as usize;
            if idx >= segments.len() {
                return Err(VerifyFailure::Truncation);
            }
            if segments[idx].chain_anchor != anchor.head_anchor
                || segments[idx].signature != anchor.head_signature
            {
                return Err(VerifyFailure::Tampering);
            }
        }

        Ok(())
    }

    /// Read surface: answers what an agent did.
    #[must_use]
    pub fn query_by_agent(&self, agent_id: &str) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| r.agent_id == agent_id)
            .collect()
    }

    /// Read surface: answers what happened in a mission.
    #[must_use]
    pub fn query_by_mission(&self, mission_id: &str) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| r.mission_id == mission_id)
            .collect()
    }

    /// Read surface: answers which actions of a kind ran in a given time window.
    #[must_use]
    pub fn query_by_kind_and_window(
        &self,
        kind: &str,
        start_unix: u64,
        end_unix: u64,
    ) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| {
                r.kind == kind && r.timestamp_unix >= start_unix && r.timestamp_unix <= end_unix
            })
            .collect()
    }

    /// Cost accounting: sums the total cost in micro-units across a collection of action records.
    #[must_use]
    pub fn total_cost_micros(records: &[&ActionRecord]) -> u64 {
        records.iter().map(|r| r.cost_micros).sum()
    }
}
