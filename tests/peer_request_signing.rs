#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Integration test for member-signed peer requests (`X-Susi-*`
//! headers): once a member's pubkey is bound in the receiver's roster,
//! Ed25519 proof-of-possession replaces the sniffable shared bearer —
//! the signature authorizes on its own, bearer-only calls from the
//! member's registered address are refused, and nonces can't replay.

use std::net::IpAddr;
use std::path::PathBuf;
use susi_config::cluster_key;
use susi_core::commit_log;
use susi_core::net_guard::{NetGuard, SignedRequest};

/// Env-seamed HOME/XDG; restores prior values on drop.
struct HomeGuard {
    tmp: PathBuf,
    prev_home: Option<std::ffi::OsString>,
    prev_userprofile: Option<std::ffi::OsString>,
    prev_xdg: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new() -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_rsig_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".susi")).unwrap();
        let g = Self {
            prev_home: std::env::var_os("HOME"),
            prev_userprofile: std::env::var_os("USERPROFILE"),
            prev_xdg: std::env::var_os("XDG_CONFIG_HOME"),
            tmp,
        };
        std::env::set_var("HOME", &g.tmp);
        std::env::set_var("USERPROFILE", &g.tmp);
        std::env::set_var("XDG_CONFIG_HOME", g.tmp.join("xdg"));
        g
    }
    fn config(&self) -> PathBuf {
        self.tmp.join(".susi")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        match &self.prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

const UNSIGNED: SignedRequest<'static> = SignedRequest {
    node: None,
    ts_secs: None,
    nonce: None,
    sig: None,
};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A `SignedRequest` as a member's headers present it.
fn signed<'a>(node: &'a str, nonce: &'a str, ts: u64, sig: &'a str) -> SignedRequest<'a> {
    SignedRequest {
        node: Some(node),
        ts_secs: Some(ts),
        nonce: Some(nonce),
        sig: Some(sig),
    }
}

fn sign(node: &str, ts: u64, nonce: &str, method: &str, path: &str) -> String {
    cluster_key::member_sign(&format!(
        "susi-peer-req-v1:{node}:{ts}:{nonce}:{method}:{path}"
    ))
    .expect("node.key must sign")
}

#[allow(clippy::too_many_arguments)] // mirrors the signed-request tuple
fn sign_v2(node: &str, ts: u64, nonce: &str, method: &str, path: &str, body: &[u8]) -> String {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(body));
    cluster_key::member_sign(&format!(
        "susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{hash}"
    ))
    .expect("node.key must sign")
}

fn write_roster(dir: &std::path::Path, node_id: &str, addr: &str, pubkey: &str) {
    let mut row = serde_json::json!({
        "node_id": node_id,
        "address": addr,
        "node_type": "PEER",
        "admission": "explicit",
    });
    if !pubkey.is_empty() {
        row["pubkey"] = serde_json::json!(pubkey);
        row["key_bound_at"] = serde_json::json!(0u64);
    }
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row]).unwrap(),
    )
    .unwrap();
}

#[test]
fn bound_member_authenticates_by_signature_and_loses_bearer() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xCCu8; 32])).unwrap();

    let self_pk = cluster_key::node_pubkey_hex().expect("node key must generate");
    let self_id = cluster_key::wire_node_id();
    let peer: IpAddr = "10.0.0.9".parse().unwrap();
    write_roster(&dir, &self_id, "10.0.0.9:9090", &self_pk);

    // A validly signed request authorizes with no bearer at all.
    let ts = now();
    let sig = sign(&self_id, ts, "nonce-1", "POST", "/mcp");
    let req = signed(&self_id, "nonce-1", ts, &sig);
    assert!(NetGuard::is_authorized(
        None, peer, &req, "POST", "/mcp", None
    ));

    // Nonce replay: the identical request is refused on the second pass.
    assert!(!NetGuard::is_authorized(
        None, peer, &req, "POST", "/mcp", None
    ));

    // A signature minted for a different path does not carry over.
    let sig2 = sign(&self_id, ts, "nonce-2", "POST", "/other");
    let req2 = signed(&self_id, "nonce-2", ts, &sig2);
    assert!(!NetGuard::is_authorized(
        None, peer, &req2, "POST", "/mcp", None
    ));

    // A signature arriving from the wrong source IP is refused — the
    // sig is genuine but the member must call from its registered
    // address.
    let sig3 = sign(&self_id, ts, "nonce-3", "POST", "/mcp");
    let req3 = signed(&self_id, "nonce-3", ts, &sig3);
    let roaming: IpAddr = "10.0.0.77".parse().unwrap();
    assert!(!NetGuard::is_authorized(
        None, roaming, &req3, "POST", "/mcp", None
    ));

    // A stale timestamp is refused.
    let old = ts.saturating_sub(3600);
    let sig4 = sign(&self_id, old, "nonce-4", "POST", "/mcp");
    let req4 = signed(&self_id, "nonce-4", old, &sig4);
    assert!(!NetGuard::is_authorized(
        None, peer, &req4, "POST", "/mcp", None
    ));

    // The bound member's shared bearer no longer suffices from its
    // registered address — it must sign.
    let bearer = cluster_key::peer_bearer().expect("cluster.key exists");
    let auth = format!("Bearer {bearer}");
    assert!(!NetGuard::is_authorized(
        Some(&auth),
        peer,
        &UNSIGNED,
        "POST",
        "/mcp",
        None
    ));
}

#[test]
fn v2_body_bound_signature_rejects_substituted_content() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xCEu8; 32])).unwrap();

    let self_pk = cluster_key::node_pubkey_hex().expect("node key must generate");
    let self_id = cluster_key::wire_node_id();
    let peer: IpAddr = "10.0.0.9".parse().unwrap();
    write_roster(&dir, &self_id, "10.0.0.9:9090", &self_pk);

    // A v2 signature minted over body A verifies for exactly those bytes.
    let ts = now();
    let body_a = br#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#;
    let sig = sign_v2(&self_id, ts, "v2-n1", "POST", "/mcp", body_a);
    let req = signed(&self_id, "v2-n1", ts, &sig);
    assert!(NetGuard::is_authorized(
        None,
        peer,
        &req,
        "POST",
        "/mcp",
        Some(body_a.as_slice())
    ));

    // Same headers, different body — an on-path relay's substitution
    // fails verification and falls through to the bearer rules.
    let body_b = br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"evil":true}}"#;
    let req2 = signed(&self_id, "v2-n2", ts, &sig);
    assert!(!NetGuard::is_authorized(
        None,
        peer,
        &req2,
        "POST",
        "/mcp",
        Some(body_b.as_slice())
    ));
}

#[test]
fn unbound_member_keeps_bearer_and_unknown_signers_fall_through() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xDDu8; 32])).unwrap();

    // Legacy member: rostered but no bound pubkey → bearer still works.
    write_roster(&dir, "node-legacy", "10.0.0.10:9090", "");
    let legacy: IpAddr = "10.0.0.10".parse().unwrap();
    let bearer = cluster_key::peer_bearer().expect("cluster.key exists");
    let auth = format!("Bearer {bearer}");
    assert!(NetGuard::is_authorized(
        Some(&auth),
        legacy,
        &UNSIGNED,
        "POST",
        "/mcp",
        None
    ));

    // An unknown node's signed attempt falls through to the bearer
    // check — a forged signature grants nothing, but a valid bearer
    // still authenticates (today's pre-binding ceiling).
    let ts = now();
    let forged_sig = hex::encode([0x11u8; 64]);
    let forged = signed("node-stranger", "n-9", ts, &forged_sig);
    assert!(!NetGuard::is_authorized(
        None, legacy, &forged, "POST", "/mcp", None
    ));
    assert!(NetGuard::is_authorized(
        Some(&auth),
        legacy,
        &forged,
        "POST",
        "/mcp",
        None
    ));
}
