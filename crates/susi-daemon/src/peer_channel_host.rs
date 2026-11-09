//! The daemon's side of the sealed peer channel (`docs/peer-channel.md`).
//!
//! - **Listener.** When `peer_channel_listen` names an address (a hub, or
//!   any node another machine should reach directly), the daemon accepts
//!   channel connections there. This is the one port meant to be reachable
//!   from other machines; the contract block (9090-9095) stays on
//!   `bind_address`. Every accepted connection is checked against the
//!   privacy posture before a byte is read: `host_only` admits loopback
//!   only, `local_only` the local network only.
//! - **Uplinks.** For every hub in `peer_channel_hubs.json` the daemon keeps
//!   one outbound link open (keepalive, reconnect with backoff), so a member
//!   behind NAT, with no inbound port, can be called through its hub.
//! - **Termination.** An end-to-end session addressed to this node is handed
//!   to the GMCP surface over an in-process socket pair, tagged with the
//!   identity the session proved.

use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use susi_config::peer_channel_link::{self as link, ChannelPeer, ChannelStream};

/// How long the GMCP side of a terminated session may sit idle between
/// requests (one tool call can legitimately run for half an hour).
const SESSION_IDLE: Duration = Duration::from_secs(susi_gmcp::server::MAX_TOOL_CALL_SECS + 60);

/// Whether `ip` is on this machine or its local network: loopback, RFC 1918,
/// link-local, IPv6 unique-local or link-local. The boundary discovery and
/// gossip keep to.
pub fn is_lan_source(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Whether the posture lets this node exchange traffic with `addr` right
/// now. The same gate decides dialing and accepting: under `host_only`
/// nothing beyond loopback, under `local_only` nothing beyond the local
/// network, otherwise what egress grants allow.
fn posture_refusal(addr: &SocketAddr) -> Option<String> {
    let ip = addr.ip().to_canonical();
    let url = match ip {
        IpAddr::V4(v4) => format!("http://{v4}:{}/", addr.port()),
        IpAddr::V6(v6) => format!("http://[{v6}]:{}/", addr.port()),
    };
    susi_core::mac_policy::egress_refusal(&url)
}

/// The dial policy every channel dial in this process uses.
pub fn dial_policy(addr: &SocketAddr) -> Result<(), String> {
    posture_refusal(addr).map_or(Ok(()), Err)
}

/// Start the listener (when configured), the uplink keeper and the
/// termination hook. Called once from the daemon's endpoint setup.
pub fn start(cfg: &susi_config::SusiConfig) {
    link::install_service(serve_locally);
    let stop = Arc::new(AtomicBool::new(false));
    let keeper_stop = Arc::clone(&stop);
    let policy: link::SharedDialPolicy = Arc::new(dial_policy);
    crate::service_supervision::Supervisor::global().spawn(
        "peer-channel-uplinks",
        5,
        None,
        move || {
            let stop = Arc::clone(&keeper_stop);
            let policy = Arc::clone(&policy);
            Some(move || link::keep_uplinks(stop, policy))
        },
    );
    let Some(listen) = cfg
        .get::<String>("peer_channel_listen")
        .and_then(|raw| link::parse_listen(&raw))
    else {
        return;
    };
    let listener = match TcpListener::bind(listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[CHANNEL] cannot listen on {listen}: {e}; the peer channel accepts nothing");
            return;
        }
    };
    let advertise: Vec<String> = match cfg.get::<serde_json::Value>("peer_channel_public") {
        Some(serde_json::Value::String(one)) if !one.trim().is_empty() => vec![one],
        Some(serde_json::Value::Array(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(_) | None => Vec::new(),
    };
    eprintln!("[CHANNEL] sealed peer channel listening on {listen}");
    let allow: link::AcceptPolicy = Arc::new(|from: &SocketAddr| posture_refusal(from).is_none());
    crate::service_supervision::Supervisor::global().spawn(
        "peer-channel-listen",
        5,
        None,
        move || {
            let listener = listener.try_clone().ok()?;
            let (stop, allow, advertise) =
                (Arc::clone(&stop), Arc::clone(&allow), advertise.clone());
            Some(move || link::serve_listener(listener, stop, allow, advertise))
        },
    );
}

/// A connected loopback socket pair whose accepted end is provably the
/// other end of `client` (a local process racing to connect first to the
/// ephemeral listener is dropped, never served).
fn socket_pair() -> std::io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;
    let client = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    let want = client.local_addr()?;
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((server, from)) if from == want => {
                server.set_nonblocking(false)?;
                return Ok((client, server));
            }
            Ok((intruder, _)) => drop(intruder),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "socket pair accept timed out",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Hand a terminated session to the GMCP surface and pump its bytes.
fn serve_locally(stream: ChannelStream, peer: ChannelPeer) {
    let (local, served) = match socket_pair() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!(
                "[CHANNEL] no socket pair for {}: {e}",
                peer.identity.node_id
            );
            return;
        }
    };
    let node = peer.identity.node_id.clone();
    if susi_gmcp::server::serve_channel_connection(susi_gmcp::server::ChannelConnection {
        stream: served,
        peer,
    })
    .is_err()
    {
        eprintln!("[CHANNEL] no GMCP surface in this process; session from {node} closed");
        return;
    }
    link::bridge(stream, local, SESSION_IDLE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn lan_sources_are_loopback_private_and_link_local_only() {
        for lan in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.140",
            "172.16.0.9",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.5",
        ] {
            assert!(is_lan_source(lan.parse().unwrap()), "{lan}");
        }
        for public in ["8.8.8.8", "100.64.0.1", "2001:4860::8888", "::ffff:1.1.1.1"] {
            assert!(!is_lan_source(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_socket_pair_connects_its_two_ends() {
        let (mut a, mut b) = socket_pair().unwrap();
        a.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        b.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping");
    }
}
