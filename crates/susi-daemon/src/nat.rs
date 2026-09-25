//! NAT Traversal Abstraction (Swarm OS Bullet 38)
//!
//! Discovers this host's public address and NAT behaviour with STUN
//! (RFC 5389 Binding requests over UDP) so agents can advertise a reachable
//! multiaddr across corporate firewalls.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::RwLock;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatStatus {
    Open,
    Symmetric,
    PortRestricted,
    Unknown,
}

const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_HEADER_LEN: usize = 20;

/// Tracks the discovered public address and NAT classification.
pub struct NatManager {
    status: RwLock<NatStatus>,
    public_ip: RwLock<Option<String>>,
}

impl Default for NatManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NatManager {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(NatStatus::Unknown),
            public_ip: RwLock::new(None),
        }
    }

    /// Record a discovery result obtained out of band (e.g. operator config).
    pub fn perform_discovery(&self, public_ip: &str, status: NatStatus) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = status;
        *self.public_ip.write().unwrap_or_else(|e| e.into_inner()) = Some(public_ip.to_string());
    }

    /// Discover the public address and NAT type by sending STUN Binding
    /// requests from one local socket to up to two `servers`.
    ///
    /// Classification: a mapping equal to the local address is `Open`;
    /// different mappings for different servers is `Symmetric`; otherwise
    /// the NAT keeps one mapping per socket and is reported as
    /// `PortRestricted` (the conservative cone class — distinguishing full
    /// and restricted cones needs CHANGE-REQUEST support from the server).
    pub fn discover(&self, servers: &[SocketAddr], timeout: Duration) -> Result<NatStatus, String> {
        let first = *servers.first().ok_or("no STUN servers configured")?;
        let bind: SocketAddr = if first.is_ipv4() {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        };
        let socket = UdpSocket::bind(bind).map_err(|e| format!("bind STUN socket: {e}"))?;
        socket
            .set_read_timeout(Some(timeout))
            .map_err(|e| format!("set STUN timeout: {e}"))?;
        let local = socket
            .local_addr()
            .map_err(|e| format!("STUN local address: {e}"))?;

        let mapped: Vec<SocketAddr> = servers
            .iter()
            .take(2)
            .filter_map(|server| stun_binding(&socket, *server).ok())
            .collect();
        let Some(primary) = mapped.first().copied() else {
            return Err("no STUN server answered".to_string());
        };

        let status = if primary.port() == local.port() && !primary.ip().is_unspecified() && {
            // Reachable directly only if the mapped IP is one of ours.
            UdpSocket::bind(SocketAddr::new(primary.ip(), 0)).is_ok()
        } {
            NatStatus::Open
        } else if mapped.iter().any(|m| *m != primary) {
            NatStatus::Symmetric
        } else {
            NatStatus::PortRestricted
        };
        self.perform_discovery(&primary.ip().to_string(), status.clone());
        Ok(status)
    }

    /// Generates a valid multiaddr for external peers to reach this daemon,
    /// factoring in the discovered NAT rules.
    pub fn generate_external_multiaddr(&self, local_port: u16) -> Result<String, String> {
        let status = self.status.read().unwrap_or_else(|e| e.into_inner());
        let ip = self.public_ip.read().unwrap_or_else(|e| e.into_inner());

        if *status == NatStatus::Symmetric {
            return Err("Symmetric NAT detected. Direct P2P requires a TURN relay.".to_string());
        }

        match ip.as_ref() {
            Some(pub_ip) if pub_ip.contains(':') => Ok(format!("/ip6/{pub_ip}/tcp/{local_port}")),
            Some(pub_ip) => Ok(format!("/ip4/{pub_ip}/tcp/{local_port}")),
            None => Err("Public IP not yet discovered. Call discover first.".to_string()),
        }
    }
}

/// Send one STUN Binding request on `socket` and return the mapped address.
pub fn stun_binding(socket: &UdpSocket, server: SocketAddr) -> Result<SocketAddr, String> {
    let mut txid = [0u8; 12];
    getrandom::fill(&mut txid).map_err(|e| format!("STUN transaction id: {e}"))?;
    let mut request = Vec::with_capacity(STUN_HEADER_LEN);
    request.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    request.extend_from_slice(&0u16.to_be_bytes());
    request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&txid);
    socket
        .send_to(&request, server)
        .map_err(|e| format!("STUN send to {server}: {e}"))?;

    let mut buf = [0u8; 576];
    loop {
        let (n, from) = socket
            .recv_from(&mut buf)
            .map_err(|e| format!("STUN recv from {server}: {e}"))?;
        // Ignore stray datagrams (other servers, late replies).
        if from != server {
            continue;
        }
        match parse_binding_response(buf.get(..n).unwrap_or_default(), &txid) {
            Some(addr) => return Ok(addr),
            None => return Err(format!("malformed STUN response from {server}")),
        }
    }
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// Parse a Binding success response, preferring XOR-MAPPED-ADDRESS.
fn parse_binding_response(msg: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if be_u16(msg, 0)? != BINDING_SUCCESS
        || msg.get(4..8)? != STUN_MAGIC_COOKIE.to_be_bytes()
        || msg.get(8..20)? != txid
    {
        return None;
    }
    let body_len = usize::from(be_u16(msg, 2)?);
    let body = msg.get(STUN_HEADER_LEN..STUN_HEADER_LEN + body_len)?;

    let mut plain = None;
    let mut at = 0;
    while at + 4 <= body.len() {
        let attr = be_u16(body, at)?;
        let len = usize::from(be_u16(body, at + 2)?);
        let value = body.get(at + 4..at + 4 + len)?;
        match attr {
            ATTR_XOR_MAPPED_ADDRESS => return decode_address(value, Some(txid)),
            ATTR_MAPPED_ADDRESS => plain = decode_address(value, None),
            _ => {}
        }
        // Attributes are padded to 4-byte boundaries.
        at += 4 + len.div_ceil(4) * 4;
    }
    plain
}

/// Decode a (XOR-)MAPPED-ADDRESS value; `xor_txid` set means XOR-encoded.
fn decode_address(value: &[u8], xor_txid: Option<&[u8; 12]>) -> Option<SocketAddr> {
    let family = *value.get(1)?;
    let mut port = be_u16(value, 2)?;
    let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
    if xor_txid.is_some() {
        port ^= (STUN_MAGIC_COOKIE >> 16) as u16;
    }
    let ip = match family {
        0x01 => {
            let mut octets: [u8; 4] = value.get(4..8)?.try_into().ok()?;
            if xor_txid.is_some() {
                for (o, c) in octets.iter_mut().zip(cookie) {
                    *o ^= c;
                }
            }
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        0x02 => {
            let mut octets: [u8; 16] = value.get(4..20)?.try_into().ok()?;
            if let Some(txid) = xor_txid {
                let key = cookie.iter().chain(txid.iter());
                for (o, k) in octets.iter_mut().zip(key) {
                    *o ^= k;
                }
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal STUN server: answers each Binding request with the sender's
    /// address as XOR-MAPPED-ADDRESS, optionally rewriting the port.
    fn spawn_stun_server(port_override: Option<u16>) -> SocketAddr {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 576];
            while let Ok((n, from)) = server.recv_from(&mut buf) {
                if n < STUN_HEADER_LEN {
                    continue;
                }
                let port = port_override.unwrap_or(from.port()) ^ (STUN_MAGIC_COOKIE >> 16) as u16;
                let IpAddr::V4(ip) = from.ip() else { continue };
                let mut octets = ip.octets();
                for (o, c) in octets.iter_mut().zip(STUN_MAGIC_COOKIE.to_be_bytes()) {
                    *o ^= c;
                }
                let mut resp = Vec::new();
                resp.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
                resp.extend_from_slice(&12u16.to_be_bytes());
                resp.extend_from_slice(&buf[4..20]);
                resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
                resp.extend_from_slice(&8u16.to_be_bytes());
                resp.extend_from_slice(&[0, 0x01]);
                resp.extend_from_slice(&port.to_be_bytes());
                resp.extend_from_slice(&octets);
                let _ = server.send_to(&resp, from);
            }
        });
        addr
    }

    #[test]
    fn test_nat_discovery_and_routing() {
        let nat = NatManager::new();

        assert!(nat.generate_external_multiaddr(8080).is_err());

        nat.perform_discovery("203.0.113.5", NatStatus::Open);
        let addr = nat.generate_external_multiaddr(8080).unwrap();
        assert_eq!(addr, "/ip4/203.0.113.5/tcp/8080");

        nat.perform_discovery("203.0.113.6", NatStatus::Symmetric);
        assert!(nat.generate_external_multiaddr(8080).is_err()); // Symmetric blocks direct
    }

    #[test]
    fn stun_binding_decodes_xor_mapped_address() {
        let server = spawn_stun_server(None);
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mapped = stun_binding(&socket, server).unwrap();
        assert_eq!(mapped, socket.local_addr().unwrap());
    }

    #[test]
    fn discover_classifies_open_when_mapping_is_local() {
        let server = spawn_stun_server(None);
        let nat = NatManager::new();
        let status = nat.discover(&[server], Duration::from_secs(2)).unwrap();
        assert_eq!(status, NatStatus::Open);
        assert!(
            nat.generate_external_multiaddr(9090)
                .unwrap()
                .starts_with("/ip4/127.0.0.1/")
        );
    }

    #[test]
    fn discover_classifies_symmetric_when_mappings_differ() {
        let a = spawn_stun_server(Some(40001));
        let b = spawn_stun_server(Some(40002));
        let nat = NatManager::new();
        assert_eq!(
            nat.discover(&[a, b], Duration::from_secs(2)).unwrap(),
            NatStatus::Symmetric
        );
    }

    #[test]
    fn discover_classifies_port_restricted_for_stable_translated_mapping() {
        let a = spawn_stun_server(Some(40003));
        let b = spawn_stun_server(Some(40003));
        let nat = NatManager::new();
        assert_eq!(
            nat.discover(&[a, b], Duration::from_secs(2)).unwrap(),
            NatStatus::PortRestricted
        );
    }

    #[test]
    fn parse_rejects_wrong_transaction_id() {
        let txid = [7u8; 12];
        let mut msg = Vec::new();
        msg.extend_from_slice(&BINDING_SUCCESS.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(&[8u8; 12]);
        assert!(parse_binding_response(&msg, &txid).is_none());
    }

    #[test]
    fn discover_errors_without_servers_or_answers() {
        let nat = NatManager::new();
        assert!(nat.discover(&[], Duration::from_millis(50)).is_err());
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let err = nat
            .discover(&[silent.local_addr().unwrap()], Duration::from_millis(100))
            .unwrap_err();
        assert!(err.contains("no STUN server answered"), "{err}");
    }
}
