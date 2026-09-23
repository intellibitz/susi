//! Stable CI smoke for parser boundaries exercised by `fuzz/` targets (no nightly).

#![allow(missing_docs)]

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
