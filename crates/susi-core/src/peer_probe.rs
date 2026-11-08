//! Cryptographic peer handshake and probe: what `susi peers probe` prints
//! and the MCP `peers_probe` tool returns (zero-UI plan B4 group 9), plus
//! the handshake and endpoint proof `susi peers add` admits a member with.
//!
//! A peer is never trusted on assertion: it must answer a cluster-key-signed
//! ping with a signed pong echoing our nonce, and its claimed MCP endpoint
//! must serve the same node id.

use serde_json::{json, Value};
use susi_config::cluster_key::VerifiedPong;

/// Seconds to wait for a signed pong per ping format.
const PONG_TIMEOUT_SECS: u64 = 3;

/// The registry of verified members (`peers.json`).
fn registry_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers.json")
}

/// Peers banned by a committed member_remove.
fn banned_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers_banned.json")
}

/// Best guess at a peer's GMCP-HTTP port when the handshake didn't announce
/// one: same-config clusters share `port_offset`, so our effective port is
/// the right default; canonical 9093 is the fallback.
pub fn peer_http_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.gmcp_http_port())
        .unwrap_or(susi_paths::ports::GMCP_HTTP)
}

/// True when `ip` is this host (loopback or one of its own interfaces): a
/// UDP responder there is this node's own daemon.
fn is_self(ip: std::net::IpAddr, node_id: &str) -> bool {
    ip.is_loopback()
        || std::net::TcpListener::bind((ip, 0)).is_ok()
        || node_id == susi_config::cluster_key::wire_node_id()
}

/// The signed-ping → signed-pong handshake `peers add` and `peers probe`
/// share, directed at one host instead of LAN broadcast. `host` may carry an
/// inline `:port`; otherwise `port`, then our effective discovery port.
/// Returns the verified pong fields plus the responder's source IP.
pub fn handshake(
    host: &str,
    port: Option<u16>,
) -> Result<(VerifiedPong, std::net::IpAddr), String> {
    let (host, inline_port) = match host.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => match p.parse::<u16>() {
            Ok(n) => (h.to_string(), Some(n)),
            Err(_) => (host.to_string(), None),
        },
        _ => (host.to_string(), None),
    };
    // Same-config assumption: a peer pinged by bare host is guessed at our
    // effective discovery port; explicit `host:port` always wins.
    let port = port.or(inline_port).unwrap_or_else(|| {
        susi_config::SusiConfig::load_global()
            .map(|c| c.udp_discovery_port())
            .unwrap_or(susi_paths::ports::UDP_DISCOVERY)
    });
    let dest = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))
        .map_err(|e| format!("cannot resolve {host}:{port}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}:{port} resolves to no address"))?;
    // host_only: no discovery ping goes to another host.
    if !dest.ip().to_canonical().is_loopback() && crate::mac_policy::host_only_in_force() {
        return Err(format!(
            "[PRIVACY] host_only: no discovery ping to {dest}: host-only privacy mode confines traffic to this machine (loopback)"
        ));
    }
    // One bound ping (`SUSI_PING_SIG3`): its MAC names the source address
    // this route uses, so a copy captured on the wire admits nobody else.
    // A zero bloom is honest: the prober registers no tools. Daemons that
    // predate the bound ping answer nothing, and are reported unreachable.
    if susi_config::cluster_key::cluster_key().is_none() {
        return Err("no cluster key at ~/.susi/cluster.key — peers cannot be verified".to_string());
    }
    let attempts: Vec<(String, String)> =
        susi_config::cluster_key::signed_ping_v3("CORE", 0, "0000000000000000", dest)
            .into_iter()
            .collect();
    if attempts.is_empty() {
        return Err(format!("no route from this host to {dest}"));
    }
    let any: std::net::SocketAddr = if dest.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = std::net::UdpSocket::bind(any).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(PONG_TIMEOUT_SECS)))
        .map_err(|e| e.to_string())?;

    // 4 KiB: a signed pong carrying a full gossip roster runs past 1 KiB.
    let mut buf = [0u8; 4096];
    for (ping, nonce) in attempts {
        socket
            .send_to(ping.as_bytes(), dest)
            .map_err(|e| e.to_string())?;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(PONG_TIMEOUT_SECS);
        let mut fallback: Option<(VerifiedPong, std::net::IpAddr)> = None;
        loop {
            let (amt, src) = match socket.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e.to_string()),
            };
            let msg = String::from_utf8_lossy(&buf[..amt]);
            let Some(pong) = susi_config::cluster_key::verify_signed_pong(&msg, &nonce) else {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                continue;
            };
            // Prefer the v3 subject-attested pong: responders send v3+v2
            // together, so a verified v2 is held as a fallback while we
            // keep listening for the attestation.
            if pong.5.is_empty() {
                fallback = fallback.or(Some((pong, src.ip())));
                if std::time::Instant::now() >= deadline {
                    break;
                }
                continue;
            }
            if is_self(src.ip(), &pong.0) {
                return Err(format!(
                    "{} answered from {} — that is this node's own daemon; \
                     this command needs a remote host",
                    pong.0,
                    src.ip()
                ));
            }
            return Ok((pong, src.ip()));
        }
        // v3 never arrived — a pre-v3 responder's verified pong is still
        // authoritative for identity, just unattested.
        if let Some((pong, ip)) = fallback {
            if is_self(ip, &pong.0) {
                return Err(format!(
                    "{} answered from {ip} — that is this node's own daemon; \
                     this command needs a remote host",
                    pong.0
                ));
            }
            return Ok((pong, ip));
        }
    }
    Err(format!(
        "no signed pong from {host}:{port} — peer unreachable, not a susi daemon, \
         doesn't share this cluster.key, or predates the bound discovery ping \
         (SUSI_PING_SIG3; every member must run a build that speaks it)"
    ))
}

/// Post-handshake endpoint proof: call `cluster_status` on the attested
/// address over the signed peer channel and require the serving node's id
/// to match. Returns the peer's reported status.
pub fn verify_endpoint(node_id: &str, address: &str) -> Result<Value, String> {
    let bearer = susi_config::cluster_key::peer_bearer();
    let result = crate::mcp_client::call_tool(
        address,
        "cluster_status",
        &json!({}),
        bearer.as_deref(),
    )
    .map_err(|e| {
        format!(
            "{node_id} verified over UDP but its MCP endpoint {address} is unreachable ({e}) — not admitting a peer that can't serve"
        )
    })?;
    let status = result
        .pointer("/content/0/text")
        .and_then(|v| v.as_str())
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| json!({}));
    let serving_id = status.get("node").and_then(|v| v.as_str()).unwrap_or("");
    if serving_id != node_id {
        return Err(format!(
            "endpoint {address} serves node {serving_id} but the handshake attested {node_id} — refusing an identity-mismatched peer"
        ));
    }
    Ok(status)
}

/// True when `rows` names `node_id` or `address`.
fn names(rows: &[Value], node_id: &str, address: &str) -> bool {
    rows.iter().any(|n| {
        n.get("node_id").and_then(|v| v.as_str()) == Some(node_id)
            || n.get("address").and_then(|v| v.as_str()) == Some(address)
    })
}

/// `susi peers probe <host> [--port]`: verify a host cryptographically and
/// report what it attests and our local standing for it. Nothing is
/// persisted; admission is `susi peers add`.
pub fn probe_report(host: &str, port: Option<u16>) -> Result<Value, String> {
    let ((node_id, checksum, bloom_hex, roster, pubkey, bind_sig), ip) = handshake(host, port)?;
    let address = format!("{}:{}", ip, peer_http_port());
    let banned = names(
        &susi_config::cluster_key::read_json_rows_strict(&banned_path())?,
        &node_id,
        &address,
    );
    let known = names(
        &susi_config::cluster_key::read_json_rows_strict(&registry_path())?,
        &node_id,
        &address,
    );
    let standing = if banned {
        "BANNED — `susi peers unban` before `peers add`"
    } else if known {
        "explicit member"
    } else {
        "not a member — `susi peers add` to admit"
    };
    let endpoint = match verify_endpoint(&node_id, &address) {
        Ok(s) => json!({
            "serving": true,
            "state": s.get("state").and_then(|v| v.as_str()).unwrap_or("up"),
            "term": s.get("term").and_then(|v| v.as_u64()).unwrap_or(0),
        }),
        Err(e) => json!({ "serving": false, "error": e }),
    };
    let bloom = if bloom_hex.is_empty() || bloom_hex.chars().all(|c| c == '0') {
        "none".to_string()
    } else {
        bloom_hex
    };
    let attestation = if pubkey.is_empty() {
        "unbound (pre-PKI peer)"
    } else if bind_sig.is_empty() {
        "attested by HMAC only — pre-v3"
    } else {
        "subject-attested"
    };
    Ok(json!({
        "verified": true,
        "node_id": node_id,
        "address": address,
        "registry_checksum": checksum,
        "capability_bloom": bloom,
        "gossip_roster_members": roster.len(),
        // A public key: safe to show in full; the prefix is what the CLI prints.
        "signing_key": {
            "pubkey": pubkey,
            "prefix": pubkey.get(..16).unwrap_or(&pubkey),
            "attestation": attestation,
        },
        "mcp_endpoint": endpoint,
        "local_standing": standing,
        "member": known,
        "banned": banned,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_lookup_matches_id_or_address() {
        let rows = vec![json!({"node_id": "n1", "address": "10.0.0.4:9093"})];
        assert!(names(&rows, "n1", "x"));
        assert!(names(&rows, "other", "10.0.0.4:9093"));
        assert!(!names(&rows, "n2", "10.0.0.5:9093"));
    }

    #[test]
    fn loopback_responder_is_this_node() {
        assert!(is_self(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "remote"
        ));
    }

    #[test]
    fn peer_http_port_returns_non_zero_port_and_names_handles_empty_rows() {
        assert!(peer_http_port() > 0);
        assert!(!names(&[], "any", "127.0.0.1:9093"));
        assert!(!names(&[json!({})], "n1", "10.0.0.1:9093"));
    }

    #[test]
    fn is_self_matches_wire_node_id_even_on_remote_ip() {
        // `wire_node_id` is read twice (here and inside `is_self`) from the
        // substrate the process environment names; sibling tests swap HOME
        // under this lock, so hold it or the two reads can name two homes.
        let _env = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let local_id = susi_config::cluster_key::wire_node_id();
        let remote_ip: std::net::IpAddr = "198.51.100.42".parse().unwrap();
        assert!(is_self(remote_ip, &local_id));
        assert!(is_self(
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
            "remote-node"
        ));
    }

    #[test]
    fn registry_and_banned_paths_target_expected_json_filenames() {
        assert_eq!(
            registry_path().file_name().and_then(|n| n.to_str()),
            Some("peers.json")
        );
        assert_eq!(
            banned_path().file_name().and_then(|n| n.to_str()),
            Some("peers_banned.json")
        );
    }
}
