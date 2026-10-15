#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-202-019: The action history is tamper-evident and interrogable.
//!
//! Mastery target:
//! Each audit record is chained to its predecessor under the existing audit key,
//! a verification command detects a mutated, removed or reordered record, and the
//! chain head is anchored periodically so the history is evidence rather than a file
//! the system could rewrite. A read surface answers what an agent did, what happened
//! in a mission, which actions of a kind ran in a window, and what each cost.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use susi_gawd::audit_evidence::{AuditEvidence, AuditExport, AuditSegment, VerifyFailure};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub id: String,
    pub agent_id: String,
    pub mission_id: String,
    pub kind: String,
    pub timestamp_unix: u64,
    pub cost_micros: u64,
    pub payload: String,
    pub segment: AuditSegment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainAnchor {
    pub anchor_index: u64,
    pub head_anchor: String,
    pub head_signature: String,
    pub anchored_unix: u64,
}

#[derive(Debug, Default)]
pub struct ActionHistory {
    evidence: AuditEvidence,
    records: Vec<ActionRecord>,
    anchors: Vec<ChainAnchor>,
    anchor_interval: usize,
    signing_key_id: String,
}

impl ActionHistory {
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

    #[allow(clippy::too_many_arguments)]
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

    pub fn export_file(&self) -> AuditExport {
        self.evidence.export_file()
    }

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
    pub fn query_by_agent(&self, agent_id: &str) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| r.agent_id == agent_id)
            .collect()
    }

    /// Read surface: answers what happened in a mission.
    pub fn query_by_mission(&self, mission_id: &str) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| r.mission_id == mission_id)
            .collect()
    }

    /// Read surface: answers which actions of a kind ran in a window.
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

    /// Cost accounting: answers what each action cost.
    pub fn total_cost_micros(records: &[&ActionRecord]) -> u64 {
        records.iter().map(|r| r.cost_micros).sum()
    }
}

#[test]
fn vc_202_019_mastery() {
    let key_id = "susi-audit-key-2026";
    let secret = "super-secret-hmac-sha256-signing-token";
    let mut keys = BTreeMap::new();
    keys.insert(key_id.to_string(), secret.to_string());

    let mut history = ActionHistory::new(key_id, secret, 2);

    // 1. Record chained actions under existing audit key
    history.record_action(
        "AGENT-CODEX",
        "MISSION-ALPHA",
        "tool_call",
        1_700_000_000,
        500,
        "execute_tool: grep_search",
    );
    history.record_action(
        "AGENT-ANTIGRAVITY",
        "MISSION-ALPHA",
        "tool_call",
        1_700_000_010,
        1200,
        "execute_tool: replace_file_content",
    );
    history.record_action(
        "AGENT-CODEX",
        "MISSION-BETA",
        "git_commit",
        1_700_000_020,
        250,
        "git commit -m 'feat: add feature'",
    );
    history.record_action(
        "AGENT-ANTIGRAVITY",
        "MISSION-BETA",
        "tool_call",
        1_700_000_030,
        800,
        "execute_tool: run_command",
    );

    // 2. Verified clean state passes integrity check
    assert!(history.verify(&keys).is_ok());

    // 3. Periodic anchor head was captured at anchor_interval (every 2 records)
    assert_eq!(history.anchors.len(), 2);
    assert_eq!(history.anchors[0].anchor_index, 1);
    assert_eq!(history.anchors[1].anchor_index, 3);

    // 4. Read surface: what an agent did
    let codex_actions = history.query_by_agent("AGENT-CODEX");
    assert_eq!(codex_actions.len(), 2);
    assert_eq!(codex_actions[0].kind, "tool_call");
    assert_eq!(codex_actions[1].kind, "git_commit");
    assert_eq!(ActionHistory::total_cost_micros(&codex_actions), 750);

    let antigravity_actions = history.query_by_agent("AGENT-ANTIGRAVITY");
    assert_eq!(antigravity_actions.len(), 2);
    assert_eq!(ActionHistory::total_cost_micros(&antigravity_actions), 2000);

    // 5. Read surface: what happened in a mission
    let alpha_actions = history.query_by_mission("MISSION-ALPHA");
    assert_eq!(alpha_actions.len(), 2);
    assert_eq!(ActionHistory::total_cost_micros(&alpha_actions), 1700);

    let beta_actions = history.query_by_mission("MISSION-BETA");
    assert_eq!(beta_actions.len(), 2);
    assert_eq!(ActionHistory::total_cost_micros(&beta_actions), 1050);

    // 6. Read surface: which actions of a kind ran in a window and what each cost
    let tool_calls_in_window =
        history.query_by_kind_and_window("tool_call", 1_700_000_005, 1_700_000_025);
    assert_eq!(tool_calls_in_window.len(), 1);
    assert_eq!(
        tool_calls_in_window[0].payload,
        "execute_tool: replace_file_content"
    );
    assert_eq!(tool_calls_in_window[0].cost_micros, 1200);

    // 7. Refutation: mutation is detected as Tampering
    let mut mutated_export = history.evidence.export();
    mutated_export[1].redacted_body = "malicious payload overwrite".to_string();
    assert_eq!(
        AuditEvidence::verify(&mutated_export, &keys),
        Err(VerifyFailure::Tampering)
    );

    // 8. Refutation: record removal (middle omission) is detected as MissingSegment
    let mut omitted_export = history.evidence.export();
    omitted_export.remove(1);
    assert_eq!(
        AuditEvidence::verify(&omitted_export, &keys),
        Err(VerifyFailure::MissingSegment)
    );

    // 9. Refutation: record reordering is detected as Tampering / index gap
    let mut reordered_export = history.evidence.export();
    reordered_export.swap(1, 2);
    assert!(AuditEvidence::verify(&reordered_export, &keys).is_err());

    // 10. Refutation: truncation is detected against known length
    let truncated_export = &history.evidence.export()[..2];
    assert_eq!(
        AuditEvidence::verify_against_length(truncated_export, &keys, 4),
        Err(VerifyFailure::Truncation)
    );

    // 11. Refutation: unknown or incorrect key is detected
    let mut wrong_keys = BTreeMap::new();
    wrong_keys.insert(key_id.to_string(), "wrong-secret".to_string());
    assert_eq!(history.verify(&wrong_keys), Err(VerifyFailure::Tampering));

    let empty_keys = BTreeMap::new();
    assert_eq!(history.verify(&empty_keys), Err(VerifyFailure::UnknownKey));
}
