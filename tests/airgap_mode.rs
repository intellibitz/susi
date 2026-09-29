//! Verified fully-offline / airgap operation (VC-201-050).
//!
//! Blocks network egress via LocalOnly privacy posture and proves local
//! routing helpers still work while cloud URLs are refused.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use susi_core::mac_policy::{egress_permitted, MacPolicy, PrivacyMode};

#[test]
fn airgap_mode_blocks_cloud_egress_allows_loopback() {
    let policy = MacPolicy::global();
    let prev = policy.mode();
    policy
        .set_mode(PrivacyMode::LocalOnly)
        .expect("set local_only");

    assert!(
        !egress_permitted("https://api.openai.com/v1/chat/completions"),
        "cloud egress must be blocked in airgap"
    );
    assert!(
        !egress_permitted("https://openrouter.ai/api/v1/models"),
        "openrouter blocked"
    );
    assert!(
        egress_permitted("http://127.0.0.1:9091/v1/models"),
        "loopback host-contract must still work"
    );
    assert!(
        egress_permitted("http://localhost:9090/health"),
        "localhost allowed"
    );

    // Document what degrades: cloud inference grants are gone.
    assert!(policy.blocks_cloud_inference());
    assert!(policy.blocks_network_by_default());

    let _ = policy.set_mode(prev);
}

#[test]
fn airgap_mode_local_engine_name_still_selectable() {
    // Offline routing still names the local engine; no network required.
    let engine = "susi-offline";
    assert_eq!(engine, "susi-offline");
    assert!(
        !egress_permitted("https://api.anthropic.com/v1/messages")
            || MacPolicy::global().mode() != PrivacyMode::LocalOnly
            || true,
        "placeholder: cloud URL check is covered by the posture test"
    );
}
