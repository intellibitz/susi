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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn new_action_history_starts_empty_and_clamps_anchor_interval_to_one() {
        let hist = ActionHistory::new("k1", "secret-1", 0);
        assert!(hist.is_empty());
        assert_eq!(hist.len(), 0);
        assert_eq!(hist.anchor_interval, 1);
        assert_eq!(hist.signing_key_id, "k1");
        assert!(hist.records().is_empty());
        assert!(hist.anchors().is_empty());
    }

    #[test]
    fn record_action_chains_segments_and_emits_periodic_chain_anchors() {
        let mut hist = ActionHistory::new("k1", "secret-1", 2);
        let first = hist
            .record_action("agent-a", "m-1", "tool_call", 100, 50, "read file")
            .expect("first record")
            .clone();
        assert_eq!(first.id, "act-0-100");
        assert_eq!(hist.anchors().len(), 0);

        let second = hist
            .record_action("agent-a", "m-1", "git_commit", 110, 75, "commit fix")
            .expect("second record")
            .clone();
        assert_eq!(second.id, "act-1-110");
        assert_eq!(hist.len(), 2);
        assert_eq!(hist.anchors().len(), 1);
        assert_eq!(hist.anchors()[0].anchor_index, 1);
        assert_eq!(hist.anchors()[0].head_anchor, second.segment.chain_anchor);
    }

    #[test]
    fn verify_succeeds_for_intact_history_and_detects_tampered_anchor_or_missing_key() {
        let mut hist = ActionHistory::new("k1", "secret-1", 1);
        hist.record_action("agent-a", "m-1", "tool_call", 100, 10, "p1")
            .expect("record 1");
        hist.record_action("agent-b", "m-2", "tool_call", 200, 20, "p2")
            .expect("record 2");

        let mut keys = BTreeMap::new();
        keys.insert("k1".to_string(), "secret-1".to_string());
        assert_eq!(hist.verify(&keys), Ok(()));
        assert_eq!(hist.export_file().expected_len, 2);
        assert_eq!(hist.evidence().export().len(), 2);

        let empty_keys = BTreeMap::new();
        assert!(hist.verify(&empty_keys).is_err());

        hist.anchors[0].head_anchor = "tampered".to_string();
        assert_eq!(hist.verify(&keys), Err(VerifyFailure::Tampering));
    }

    #[test]
    fn query_surfaces_and_total_cost_micros_filter_by_agent_mission_and_window() {
        let mut hist = ActionHistory::new("k1", "secret-1", 5);
        hist.record_action("agent-a", "m-1", "tool_call", 100, 150, "a1");
        hist.record_action("agent-b", "m-1", "git_commit", 200, 250, "b1");
        hist.record_action("agent-a", "m-2", "tool_call", 300, 350, "a2");

        let by_agent = hist.query_by_agent("agent-a");
        assert_eq!(by_agent.len(), 2);
        assert_eq!(ActionHistory::total_cost_micros(&by_agent), 500);

        let by_mission = hist.query_by_mission("m-1");
        assert_eq!(by_mission.len(), 2);
        assert_eq!(ActionHistory::total_cost_micros(&by_mission), 400);

        let windowed = hist.query_by_kind_and_window("tool_call", 150, 350);
        assert_eq!(windowed.len(), 1);
        assert_eq!(windowed[0].payload, "a2");
    }

    #[test]
    fn action_record_and_chain_anchor_serde_json_roundtrip_and_detect_truncation() {
        let mut hist = ActionHistory::new("k1", "secret-1", 1);
        let rec = hist
            .record_action("agent-a", "m-1", "tool_call", 100, 10, "p1")
            .expect("record 1")
            .clone();
        let rec_json = serde_json::to_string(&rec).expect("serialize ActionRecord");
        let rec_back: ActionRecord =
            serde_json::from_str(&rec_json).expect("deserialize ActionRecord");
        assert_eq!(rec_back, rec);

        let anchor = hist.anchors()[0].clone();
        let anchor_json = serde_json::to_string(&anchor).expect("serialize ChainAnchor");
        let anchor_back: ChainAnchor =
            serde_json::from_str(&anchor_json).expect("deserialize ChainAnchor");
        assert_eq!(anchor_back, anchor);

        hist.anchors[0].anchor_index = 99;
        let keys = BTreeMap::from([("k1".to_string(), "secret-1".to_string())]);
        assert_eq!(hist.verify(&keys), Err(VerifyFailure::Truncation));
    }
}
