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

use std::collections::BTreeMap;
use susi_gawd::action_history::ActionHistory;
use susi_gawd::audit_evidence::{AuditEvidence, VerifyFailure};

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

#[test]
fn vc_202_019_empty_history_verifies_and_returns_zero_cost_queries() {
    let key_id = "audit-empty-key";
    let secret = "audit-empty-secret";
    let mut keys = BTreeMap::new();
    keys.insert(key_id.to_string(), secret.to_string());

    let history = ActionHistory::new(key_id, secret, 3);
    assert!(history.verify(&keys).is_ok());
    assert!(history.anchors.is_empty());
    assert!(history.query_by_agent("AGENT-NONE").is_empty());
    assert!(history.query_by_mission("MISSION-NONE").is_empty());
    assert!(history
        .query_by_kind_and_window("tool_call", 0, u64::MAX)
        .is_empty());
    assert_eq!(ActionHistory::total_cost_micros(&[]), 0);
}

#[test]
fn vc_202_019_window_query_respects_inclusive_timestamp_boundaries() {
    let mut history = ActionHistory::new("k-win", "s-win", 5);
    history.record_action("A1", "M1", "step", 100, 10, "first");
    history.record_action("A1", "M1", "step", 200, 20, "second");
    history.record_action("A1", "M1", "step", 300, 30, "third");

    let inclusive = history.query_by_kind_and_window("step", 100, 200);
    assert_eq!(inclusive.len(), 2);
    assert_eq!(ActionHistory::total_cost_micros(&inclusive), 30);

    let between = history.query_by_kind_and_window("step", 201, 299);
    assert!(between.is_empty());

    let wrong_kind = history.query_by_kind_and_window("other", 0, 500);
    assert!(wrong_kind.is_empty());
}

#[test]
fn vc_202_019_anchor_interval_one_captures_anchor_for_every_record() {
    let key_id = "k-every";
    let secret = "s-every";
    let mut keys = BTreeMap::new();
    keys.insert(key_id.to_string(), secret.to_string());

    let mut history = ActionHistory::new(key_id, secret, 1);
    history.record_action("A1", "M1", "op", 10, 5, "p1");
    history.record_action("A2", "M1", "op", 20, 15, "p2");
    history.record_action("A3", "M2", "op", 30, 25, "p3");

    assert!(history.verify(&keys).is_ok());
    assert_eq!(history.anchors.len(), 3);
    assert_eq!(history.anchors[0].anchor_index, 0);
    assert_eq!(history.anchors[1].anchor_index, 1);
    assert_eq!(history.anchors[2].anchor_index, 2);
}

#[test]
fn vc_202_019_verify_against_length_accepts_exact_count_and_rejects_short_export() {
    let key_id = "k-len";
    let secret = "s-len";
    let mut keys = BTreeMap::new();
    keys.insert(key_id.to_string(), secret.to_string());

    let mut history = ActionHistory::new(key_id, secret, 2);
    history.record_action("A1", "M1", "op", 10, 100, "step-1");
    history.record_action("A1", "M1", "op", 20, 200, "step-2");
    history.record_action("A2", "M1", "op", 30, 300, "step-3");

    let export = history.evidence.export();
    assert!(AuditEvidence::verify_against_length(&export, &keys, 3).is_ok());
    assert_eq!(
        AuditEvidence::verify_against_length(&export, &keys, 4),
        Err(VerifyFailure::Truncation)
    );
}
