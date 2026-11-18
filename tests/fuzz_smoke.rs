#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Stable CI smoke for parser boundaries exercised by `fuzz/` targets (no nightly).

#![allow(missing_docs)] // integration test crate: no public API to document

use susi_core::bus::SwarmEvent;

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
fn fuzz_smoke_swarm_event_roundtrips_valid_events_and_rejects_malformed_control_chars() {
    for malformed in ["{\"schema_version\":", "\x01\x02\x03\x7f"] {
        assert!(SwarmEvent::decode(malformed).is_err());
    }
    assert!(SwarmEvent::decode("[1,2,3]").unwrap().is_none());
    let valid = r#"{"schema_version":1,"AgentStarted":{"agent_name":"worker-alpha"}}"#;
    let decoded = SwarmEvent::decode(valid).unwrap();
    assert!(decoded.is_some());
}
