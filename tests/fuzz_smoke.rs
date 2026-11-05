#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Stable CI smoke for parser boundaries exercised by `fuzz/` targets (no nightly).

#![allow(missing_docs)] // integration test crate: no public API to document

use susi_core::bus::SwarmEvent;
use susi_core::evidence::{Claim, EvidenceRecord, EvidenceSource};

#[test]
fn fuzz_smoke_evidence_json_does_not_panic() {
    let garbage = [
        "",
        "{",
        "null",
        r#"{"agent_id":"x","rank":1,"timestamp":0,"claim":{"subject":"","predicate":"","value":""},"source":{"System":{"metric":"m","value":"v"}},"confidence":0.5,"signature":"deadbeef"}"#,
        "\u{fffd}\u{0000}",
    ];
    for input in garbage {
        let _: Result<EvidenceRecord, _> = serde_json::from_str(input);
    }
    let record = EvidenceRecord::new(
        "agent".into(),
        1.0,
        1,
        Claim {
            subject: "s".into(),
            predicate: "p".into(),
            value: "v".into(),
        },
        EvidenceSource::System {
            metric: "m".into(),
            value: "v".into(),
        },
        1.0,
    );
    assert!(!record.signature.is_empty());
}

#[test]
fn fuzz_smoke_swarm_event_decode_does_not_panic() {
    let garbage = [
        "",
        "not-json",
        r#"{"schema_version":999,"AgentStarted":{"agent_name":"a"}}"#,
        r#"{"schema_version":1,"AgentStarted":{"agent_name":"a"}}"#,
        "\u{0000}{}",
    ];
    for input in garbage {
        let _ = SwarmEvent::decode(input);
    }
}

#[test]
fn fuzz_smoke_evidence_record_roundtrips_json_and_handles_deep_strings() {
    let deep = "nested/".repeat(128);
    let record = EvidenceRecord::new(
        "fuzz-agent".into(),
        2.0,
        42,
        Claim {
            subject: deep.clone(),
            predicate: "verified".into(),
            value: "true\n\t\"quoted\"".into(),
        },
        EvidenceSource::System {
            metric: "latency_ms".into(),
            value: "12".into(),
        },
        0.95,
    );
    let encoded = serde_json::to_string(&record).unwrap();
    let decoded: EvidenceRecord = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.agent_id, "fuzz-agent");
    assert_eq!(decoded.claim.subject, deep);
    assert_eq!(decoded.signature, record.signature);
}

#[test]
fn fuzz_smoke_swarm_event_roundtrips_valid_events_and_rejects_malformed_control_chars() {
    for malformed in ["{\"schema_version\":", "\x01\x02\x03\x7f"] {
        assert!(SwarmEvent::decode(malformed).is_err());
    }
    assert!(SwarmEvent::decode("[1,2,3]").unwrap().is_none());
    let valid = r#"{"schema_version":1,"AgentStarted":{"agent_name":"worker-alpha"}}"#;
    let decoded = SwarmEvent::decode(valid).unwrap();
    assert!(decoded.is_some());
}

#[test]
fn fuzz_smoke_evidence_record_confidence_and_rank_are_preserved() {
    let record = EvidenceRecord::new(
        "id-rank".into(),
        2.5,
        12345,
        Claim {
            subject: "sub".into(),
            predicate: "pred".into(),
            value: "val".into(),
        },
        EvidenceSource::System {
            metric: "cpu".into(),
            value: "0.5".into(),
        },
        0.75,
    );
    assert!((record.rank - 2.5).abs() < f32::EPSILON);
    assert!((record.confidence - 0.75).abs() < f32::EPSILON);
    assert_eq!(record.timestamp, 12345);
}
