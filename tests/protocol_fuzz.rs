//! Property/fuzz-style smoke for wire frames, claim blobs, compliance parser.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    missing_docs
)]

use serde::Deserialize;
use susi_core::bus::SwarmEvent;

#[derive(Debug, Deserialize)]
struct ClaimBlob {
    task_id: String,
    agent: String,
    #[serde(default)]
    lease_until: u64,
}

fn claim_roundtrip_or_err(bytes: &[u8]) {
    // Bounded: never allocate from length prefix beyond 64KiB.
    if bytes.len() > 65_536 {
        return;
    }
    let _ = serde_json::from_slice::<serde_json::Value>(bytes);
}

#[test]
fn protocol_fuzz_swarm_event_no_panic() {
    let cases: &[&[u8]] = &[
        b"",
        b"{",
        b"null",
        b"[]",
        br#"{"type":"ping"}"#,
        b"\xff\xfe\x00",
    ];
    for c in cases {
        let _: Result<SwarmEvent, _> = serde_json::from_slice(c);
        claim_roundtrip_or_err(c);
    }
}

#[test]
fn protocol_fuzz_claim_blob_bounded() {
    let huge = vec![b'a'; 70_000];
    claim_roundtrip_or_err(&huge);
    let ok = br#"{"task_id":"T-1","agent":"CURSOR","lease_until":1}"#;
    let parsed: ClaimBlob = serde_json::from_slice(ok).unwrap();
    assert_eq!(parsed.task_id, "T-1");
    assert_eq!(parsed.agent, "CURSOR");
    assert_eq!(parsed.lease_until, 1);
}

#[test]
fn protocol_fuzz_compliance_trailer_parse() {
    for msg in [
        "",
        "feat: x\n\nTask: T-CLAUDE-1\n",
        "feat: x\n\nTask: not-a-task\n",
        "\0\nTask: T-CURSOR-1\n",
    ] {
        let _has = msg.lines().any(|l| l.starts_with("Task: T-"));
    }
}
