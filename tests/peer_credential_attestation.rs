// Integration test crate: a failed setup step is a test failure, so the
// panic-path lints are allowed for the whole file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Peer credential hardening (T-PEERSEC-1), end to end through the real MCP
//! peer client against real sockets:
//!
//! - an unattested remote peer (no node key bound to its address, like the
//!   row a discovery ping creates) receives nothing: the call fails naming
//!   the missing attestation before a connection is made;
//! - an attested remote member receives no bearer over plaintext `http://`,
//!   only the member signature, and the failure names the cross-host rule;
//! - a loopback target still receives the bearer it was handed.
//!
//! Every accept and read is bounded, so a regression fails, never hangs.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use susi_config::cluster_key;
use susi_core::commit_log;
use susi_core::mcp_client;

mod common;
use common::HomeGuard;

/// This host's non-loopback IPv4 address: the source address the OS would
/// use toward a TEST-NET destination. UDP `connect` sends nothing.
fn lan_ip() -> IpAddr {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind probe socket");
    probe
        .connect("192.0.2.1:9")
        .expect("this test needs a non-loopback IPv4 interface with a route");
    let ip = probe.local_addr().expect("probe local address").ip();
    assert!(
        !ip.is_loopback() && !ip.is_unspecified(),
        "no non-loopback IPv4 address on this host ({ip})"
    );
    ip
}

/// A one-shot fake peer on `ip`: it forwards the head of the first request
/// it receives and answers 401. It stops accepting after `window`.
fn fake_peer(ip: IpAddr, window: Duration) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind((ip, 0)).expect("bind fake peer");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let addr = listener
        .local_addr()
        .expect("fake peer address")
        .to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).expect("blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && head.len() < 64 * 1024 {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).to_ascii_lowercase());
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            return;
        }
    });
    (addr, rx)
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
        row["key_bound_at"] = serde_json::json!(1u64);
    }
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row]).unwrap(),
    )
    .unwrap();
}

fn isolated_cluster_home(tag: &str) -> HomeGuard {
    let home = HomeGuard::new(tag);
    std::fs::write(home.config().join("cluster.key"), hex::encode([0x5Au8; 32])).unwrap();
    home
}

#[test]
fn an_unattested_peer_never_receives_the_bearer() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = isolated_cluster_home("pca-unattested");
    let ip = lan_ip();
    let (addr, heads) = fake_peer(ip, Duration::from_secs(2));
    // The row a discovery ping creates: Explicit, synthetic id, no key.
    write_roster(&home.config(), &format!("susi-peer-{ip}"), &addr, "");
    let bearer = cluster_key::peer_bearer().expect("cluster.key yields the peer bearer");

    let refused = mcp_client::credential_for_target(&addr, Some(&bearer)).unwrap_err();
    assert!(refused.contains("not attested"), "{refused}");
    let err = mcp_client::call_tool(
        &addr,
        "cluster_status",
        &serde_json::json!({}),
        Some(&bearer),
    )
    .unwrap_err();
    assert!(err.contains("not attested"), "{err}");
    assert!(
        heads.recv_timeout(Duration::from_millis(800)).is_err(),
        "the unattested peer must not even see a connection"
    );
}

#[test]
fn no_bearer_crosses_plaintext_http_to_a_remote_member() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = isolated_cluster_home("pca-attested");
    let ip = lan_ip();
    let (addr, heads) = fake_peer(ip, Duration::from_secs(10));
    // Attested: a node key is bound to the address. Our own key stands in
    // for the member's, so the sealed body is one we could open.
    let member_pk = cluster_key::node_pubkey_hex().expect("node.key generates");
    write_roster(&home.config(), "member-b", &addr, &member_pk);
    let bearer = cluster_key::peer_bearer().expect("cluster.key yields the peer bearer");

    assert_eq!(
        mcp_client::credential_for_target(&addr, Some(&bearer)),
        Ok(None),
        "an attested remote member authenticates by signature, not bearer"
    );
    let err = mcp_client::call_tool(
        &addr,
        "cluster_status",
        &serde_json::json!({}),
        Some(&bearer),
    )
    .unwrap_err();
    assert!(err.contains("HTTP 401"), "{err}");
    assert!(err.contains("cross-host peer calls"), "{err}");
    let head = heads
        .recv_timeout(Duration::from_secs(8))
        .expect("the attested member receives the request");
    assert!(head.starts_with("post /mcp"), "{head}");
    assert!(!head.contains("authorization:"), "bearer leaked: {head}");
    assert!(
        !head.contains(&bearer.to_ascii_lowercase()),
        "bearer value leaked: {head}"
    );
    assert!(head.contains("x-susi-req-sig:"), "member signature: {head}");
    assert!(head.contains("x-susi-enc: v1"), "sealed body: {head}");
}

#[test]
fn a_loopback_target_still_receives_its_bearer() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _home = isolated_cluster_home("pca-loopback");
    let (addr, heads) = fake_peer(IpAddr::from([127, 0, 0, 1]), Duration::from_secs(10));
    assert_eq!(
        mcp_client::credential_for_target(&addr, Some("host-token")),
        Ok(Some("host-token"))
    );
    assert!(mcp_client::is_loopback_target("localhost:9093"));
    assert!(mcp_client::is_loopback_target("[::1]:9093"));
    assert!(!mcp_client::is_loopback_target("192.168.1.4:9093"));
    assert!(!mcp_client::is_loopback_target("[fe80::1]:9093"));

    let err = mcp_client::call_tool(
        &addr,
        "cluster_status",
        &serde_json::json!({}),
        Some("host-token"),
    )
    .unwrap_err();
    assert!(err.contains("HTTP 401"), "{err}");
    assert!(
        !err.contains("cross-host"),
        "loopback carries no note: {err}"
    );
    let head = heads
        .recv_timeout(Duration::from_secs(8))
        .expect("the loopback peer receives the request");
    assert!(
        head.contains("authorization: bearer host-token"),
        "loopback keeps its bearer: {head}"
    );
}
