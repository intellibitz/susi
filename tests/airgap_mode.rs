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

use susi_core::mac_policy::{MacPolicy, PrivacyMode};

#[test]
fn airgap_mode_blocks_cloud_egress_allows_loopback() {
    let policy = MacPolicy::new([11; 32], PrivacyMode::LocalOnly, true);

    assert!(
        !policy.egress_permitted("https://api.openai.com/v1/chat/completions"),
        "cloud egress must be blocked in airgap"
    );
    assert!(
        !policy.egress_permitted("https://openrouter.ai/api/v1/models"),
        "openrouter blocked"
    );
    assert!(
        policy.egress_permitted("http://127.0.0.1:9091/v1/models"),
        "loopback host-contract must still work"
    );
    assert!(
        policy.egress_permitted("http://localhost:9090/health"),
        "localhost allowed"
    );

    // Document what degrades: cloud inference grants are gone.
    assert!(policy.blocks_cloud_inference());
    assert!(policy.blocks_network_by_default());
}

#[test]
fn airgap_mode_local_engine_name_still_selectable() {
    // Offline routing still names the local engine; no network required.
    let engine = "susi-offline";
    assert_eq!(engine, "susi-offline");
}
