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
fn udp_9092_call_sites_never_send_retired_lan_ping_or_broadcast_host_token() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "crates/susi-daemon/src/server.rs",
        "crates/susi-gawd/src/kernel_loader.rs",
        "crates/susi-gawd-swarm/src/amas.rs",
    ] {
        let full = root.join(rel);
        let raw = std::fs::read_to_string(&full)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", full.display()));
        let prod = production_prefix(&raw);
        assert!(
            !prod.contains("b\"SUSI_LAN_PING\"") && !prod.contains("\"SUSI_LAN_PING\""),
            "{rel} production code still references retired unauthenticated SUSI_LAN_PING literal"
        );
    }

    let amas_raw = std::fs::read_to_string(root.join("crates/susi-gawd-swarm/src/amas.rs"))
        .expect("read crates/susi-gawd-swarm/src/amas.rs");
    let amas_prod = production_prefix(&amas_raw);
    let discovery_fn = amas_prod
        .split("pub fn list_cluster_nodes()")
        .nth(1)
        .and_then(|s| s.split("fn rank_delta_for_output").next())
        .expect("locate list_cluster_nodes in crates/susi-gawd-swarm/src/amas.rs");
    assert!(
        !discovery_fn.contains("host_token"),
        "crates/susi-gawd-swarm/src/amas.rs::list_cluster_nodes must never broadcast or read host_token on UDP discovery"
    );
    assert!(
        discovery_fn.contains("signed_lan_discovery_frame("),
        "crates/susi-gawd-swarm/src/amas.rs::list_cluster_nodes must use signed_lan_discovery_frame for UDP 9092 discovery"
    );
    assert!(
        discovery_fn.contains("route_source_ip"),
        "each discovery ping must be bound to the source address of its route"
    );
}

#[test]
fn daemon_udp_discovery_listener_and_bootloader_probe_enforce_signed_ping_sig() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let server_raw = std::fs::read_to_string(root.join("crates/susi-daemon/src/server.rs"))
        .expect("read crates/susi-daemon/src/server.rs");
    let server_prod = production_prefix(&server_raw);
    assert!(
        server_prod.contains("starts_with(b\"SUSI_PING_SIG\")"),
        "crates/susi-daemon/src/server.rs UDP listener must gate on starts_with(b\"SUSI_PING_SIG\")"
    );
    assert!(
        server_prod.contains("verify_bound_ping(&msg, src)"),
        "crates/susi-daemon/src/server.rs UDP listener must verify the bound ping against its datagram source"
    );

    let loader_raw = std::fs::read_to_string(root.join("crates/susi-gawd/src/kernel_loader.rs"))
        .expect("read crates/susi-gawd/src/kernel_loader.rs");
    let loader_prod = production_prefix(&loader_raw);
    assert!(
        loader_prod.contains("cluster_key::signed_ping_v3")
            && loader_prod.contains("cluster_key::verify_signed_pong"),
        "crates/susi-gawd/src/kernel_loader.rs must probe UDP 9092 with cluster_key::signed_ping_v3 and verify_signed_pong"
    );
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
    for rel in [
        "crates/susi-config/src/cluster_key.rs",
        "crates/susi-core/src/peer_probe.rs",
        "crates/susi-daemon/src/server.rs",
        "crates/susi-gawd/src/kernel_loader.rs",
        "crates/susi-gawd-swarm/src/amas.rs",
    ] {
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
