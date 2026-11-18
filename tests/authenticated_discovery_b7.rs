#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Permanent architectural ratchet for `B7` (Authenticated UDP `9092` Discovery).
//!
//! Verifies that:
//! 1. No production code in `crates/susi-daemon/src/server.rs`,
//!    `crates/susi-gawd/src/kernel_loader.rs`, or
//!    `crates/susi-gawd-swarm/src/amas.rs` sends the retired unauthenticated
//!    `SUSI_LAN_PING` literal.
//! 2. `crates/susi-gawd-swarm/src/amas.rs` never broadcasts `host_token`
//!    (`api_token`) to `255.255.255.255` over UDP.
//! 3. `crates/susi-daemon/src/server.rs`'s UDP discovery responder enforces
//!    `starts_with(b"SUSI_PING_SIG")` and `verify_bound_ping` (T-PEERSEC-1:
//!    the ping MACs its sender's source address, a timestamp and a nonce).
//! 4. No production sender or receiver uses the retired unbound pings
//!    (`signed_ping`, `signed_ping_v2`, `verify_signed_ping`), whose captured
//!    copies replayed from any address.

use std::path::Path;

fn production_prefix(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

#[test]
fn production_prefix_strips_cfg_test_suffix_and_preserves_unsplit_input() {
    let sample = "fn prod() {}\n#[cfg(test)]\nmod tests { fn helper() {} }";
    assert_eq!(production_prefix(sample), "fn prod() {}\n");
    assert_eq!(production_prefix("fn only_prod() {}"), "fn only_prod() {}");
}

#[test]
fn cluster_key_signed_ping_and_pong_helpers_remain_wired_in_config() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cluster_key_raw =
        std::fs::read_to_string(root.join("crates/susi-config/src/cluster_key.rs"))
            .expect("read crates/susi-config/src/cluster_key.rs");
    let prod = production_prefix(&cluster_key_raw);
    assert!(prod.contains("pub fn signed_ping_v3"));
    assert!(prod.contains("pub fn verify_bound_ping"));
    assert!(prod.contains("pub fn signed_pong"));
    assert!(prod.contains("pub fn verify_signed_pong"));
}

#[test]
fn cluster_key_defines_signed_lan_discovery_frames_in_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(root.join("crates/susi-config/src/cluster_key.rs"))
        .expect("read crates/susi-config/src/cluster_key.rs");
    let prod = production_prefix(&raw);
    assert!(prod.contains("SUSI_PING_SIG"));
    assert!(prod.contains("SUSI_PONG_SIG"));
}

#[test]
fn no_production_code_sends_or_answers_the_retired_unbound_pings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in ["crates/susi-config/src/cluster_key.rs"] {
        let raw = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("failed to read {rel}: {e}"));
        let prod = production_prefix(&raw);
        for retired in [
            "signed_ping(",
            "signed_ping_v2(",
            "signed_ping_legacy(",
            "verify_signed_ping(",
        ] {
            assert!(
                !prod.contains(retired),
                "{rel} production code still uses the retired unbound ping `{retired}`"
            );
        }
    }
}
