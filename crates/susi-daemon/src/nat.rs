//! NAT Traversal Abstraction
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
    /// Public path is a TURN relay (XOR-RELAYED-ADDRESS or operator `SUSI_TURN_RELAY`).
    TurnRelayed,
    Unknown,
}

const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const ATTR_XOR_RELAYED_ADDRESS: u16 = 0x0016;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_REALM: u16 = 0x0014;
const ATTR_NONCE: u16 = 0x0015;
const ATTR_USERNAME: u16 = 0x0006;
const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;
const TURN_ALLOCATE: u16 = 0x0003;
const TURN_ALLOCATE_SUCCESS: u16 = 0x0103;
const TURN_ALLOCATE_ERROR: u16 = 0x0113;
const STUN_HEADER_LEN: usize = 20;
const STUN_MI_LEN: usize = 20;
const UDP_TRANSPORT: u8 = 17;

/// Tracks the discovered public address and NAT classification.
pub struct NatManager {
    status: RwLock<NatStatus>,
    public_ip: RwLock<Option<String>>,
    last_error: RwLock<Option<String>>,
    turn_relay: RwLock<Option<String>>,
}

impl Default for NatManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NatManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: RwLock::new(NatStatus::Unknown),
            public_ip: RwLock::new(None),
            last_error: RwLock::new(None),
            turn_relay: RwLock::new(None),
        }
    }

    /// Record a discovery result obtained out of band (e.g. operator config).
    pub fn perform_discovery(&self, public_ip: &str, status: NatStatus) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = status;
        *self.public_ip.write().unwrap_or_else(|e| e.into_inner()) = Some(public_ip.to_string());
        *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Record a STUN/TURN failure without pretending the mapping is Open.
    pub fn record_error(&self, error: impl Into<String>) {
        *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = Some(error.into());
    }

    /// Last STUN/TURN error, if discovery failed or stayed Unknown.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Relayed host:port advertised when STUN classified Symmetric or failed.
    pub fn turn_relay(&self) -> Option<String> {
        self.turn_relay
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_turn_relay(&self, relay: SocketAddr) {
        *self.turn_relay.write().unwrap_or_else(|e| e.into_inner()) = Some(relay.to_string());
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = NatStatus::TurnRelayed;
        *self.public_ip.write().unwrap_or_else(|e| e.into_inner()) = Some(relay.ip().to_string());
        *self.last_error.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Discover the public address and NAT type by sending STUN Binding
    /// requests from one local socket to up to two `servers`.
    ///
    /// Classification: a mapping equal to the local address is `Open`;
    /// different mappings for different servers is `Symmetric`; otherwise
    /// the NAT keeps one mapping per socket and is reported as
    /// `PortRestricted` (the conservative cone class — distinguishing full
    /// and restricted cones needs CHANGE-REQUEST support from the server).
    ///
    /// # Errors
    /// Fails when no servers are given, the socket cannot be set up, or no
    /// server answers within `timeout`.
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

    /// Resolve public STUN servers and classify this host (2s per server).
    ///
    /// # Errors
    /// Fails when DNS yields no addresses or no server answers.
    pub fn discover_default(&self) -> Result<NatStatus, String> {
        use std::net::ToSocketAddrs;
        const SERVERS: [&str; 2] = ["stun.l.google.com:19302", "stun.cloudflare.com:3478"];
        let mut addrs = Vec::new();
        for host in SERVERS {
            if let Ok(iter) = host.to_socket_addrs() {
                addrs.extend(iter.take(1));
            }
        }
        if addrs.is_empty() {
            let err = "STUN DNS produced no addresses".to_string();
            self.record_error(&err);
            return Err(err);
        }
        match self.discover(&addrs, Duration::from_secs(2)) {
            Ok(status) => Ok(status),
            Err(e) => {
                self.record_error(&e);
                Err(e)
            }
        }
    }

    /// TURN Allocate (RFC 5766) against `SUSI_TURN_SERVER`, or adopt
    /// `SUSI_TURN_RELAY` as an already-allocated relay. Does not invent a
    /// mapping when neither is set. Long-term credentials (RFC 5389
    /// MESSAGE-INTEGRITY HMAC-SHA1) use `SUSI_TURN_USER` / `SUSI_TURN_PASS`
    /// and the 401 REALM/NONCE (`SUSI_TURN_REALM` overrides an empty realm).
    ///
    /// # Errors
    /// Fails when no TURN config is present, DNS fails, Allocate is rejected,
    /// or a 401 arrives without operator credentials.
    pub fn allocate_turn_from_env(&self) -> Result<SocketAddr, String> {
        if let Ok(relay) = std::env::var("SUSI_TURN_RELAY") {
            let relay = relay.trim();
            if !relay.is_empty() {
                let addr: SocketAddr = relay
                    .parse()
                    .map_err(|e| format!("SUSI_TURN_RELAY parse: {e}"))?;
                self.set_turn_relay(addr);
                return Ok(addr);
            }
        }
        let server = std::env::var("SUSI_TURN_SERVER").map_err(|_| {
            "no TURN path: set SUSI_TURN_SERVER=host:port or SUSI_TURN_RELAY=ip:port".to_string()
        })?;
        let server = server.trim();
        if server.is_empty() {
            return Err("SUSI_TURN_SERVER is empty".to_string());
        }
        use std::net::ToSocketAddrs;
        let addr = server
            .to_socket_addrs()
            .map_err(|e| format!("TURN DNS: {e}"))?
            .next()
            .ok_or_else(|| "TURN DNS produced no addresses".to_string())?;
        match turn_allocate(addr, Duration::from_secs(2), turn_long_term_creds()) {
            Ok(relayed) => {
                self.set_turn_relay(relayed);
                Ok(relayed)
            }
            Err(e) => {
                self.record_error(format!("TURN Allocate failed ({e})"));
                Err(e)
            }
        }
    }

    /// Last classified NAT behaviour (`Unknown` until discovery).
    pub fn status(&self) -> NatStatus {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Last discovered public IP, if any.
    pub fn public_ip(&self) -> Option<String> {
        self.public_ip
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Generates a valid multiaddr for external peers to reach this daemon,
    /// factoring in the discovered NAT rules.
    ///
    /// # Errors
    /// Fails behind a symmetric NAT or before a public IP is known.
    pub fn generate_external_multiaddr(&self, local_port: u16) -> Result<String, String> {
        let status = self.status.read().unwrap_or_else(|e| e.into_inner());
        let ip = self.public_ip.read().unwrap_or_else(|e| e.into_inner());

        if *status == NatStatus::Symmetric {
            if let Some(relay) = self
                .turn_relay
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                return Ok(turn_multiaddr(relay));
            }
            return Err("Symmetric NAT detected. Direct P2P requires a TURN relay.".to_string());
        }

        if *status == NatStatus::TurnRelayed {
            if let Some(relay) = self
                .turn_relay
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                return Ok(turn_multiaddr(relay));
            }
        }

        match ip.as_ref() {
            Some(pub_ip) if pub_ip.contains(':') => Ok(format!("/ip6/{pub_ip}/tcp/{local_port}")),
            Some(pub_ip) => Ok(format!("/ip4/{pub_ip}/tcp/{local_port}")),
            None => Err("Public IP not yet discovered. Call discover first.".to_string()),
        }
    }
}

/// Send one STUN Binding request on `socket` and return the mapped address.
///
/// # Errors
/// Fails on send/receive errors (including timeout) or a malformed reply.
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
        match parse_stun_success(
            buf.get(..n).unwrap_or_default(),
            &txid,
            BINDING_SUCCESS,
            ATTR_XOR_MAPPED_ADDRESS,
            ATTR_MAPPED_ADDRESS,
        ) {
            Some(addr) => return Ok(addr),
            None => return Err(format!("malformed STUN response from {server}")),
        }
    }
}

fn turn_multiaddr(relay: &str) -> String {
    match relay.parse::<SocketAddr>() {
        Ok(sa) => match sa.ip() {
            IpAddr::V4(ip) => format!("/ip4/{ip}/udp/{}/turn", sa.port()),
            IpAddr::V6(ip) => format!("/ip6/{ip}/udp/{}/turn", sa.port()),
        },
        Err(_) => format!("/udp/{relay}/turn"),
    }
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// Parse a STUN/TURN success response, preferring `xor_attr` then `plain_attr`.
#[cfg(test)]
fn parse_binding_response(msg: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    parse_stun_success(
        msg,
        txid,
        BINDING_SUCCESS,
        ATTR_XOR_MAPPED_ADDRESS,
        ATTR_MAPPED_ADDRESS,
    )
}

fn parse_stun_success(
    msg: &[u8],
    txid: &[u8; 12],
    success_type: u16,
    xor_attr: u16,
    plain_attr: u16,
) -> Option<SocketAddr> {
    if be_u16(msg, 0)? != success_type
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
        if attr == xor_attr {
            return decode_address(value, Some(txid));
        }
        if attr == plain_attr {
            plain = decode_address(value, None);
        }
        // Attributes are padded to 4-byte boundaries.
        at += 4 + len.div_ceil(4) * 4;
    }
    plain
}

struct TurnCreds {
    user: String,
    pass: String,
    realm_override: Option<String>,
}

fn turn_long_term_creds() -> Option<TurnCreds> {
    let user = std::env::var("SUSI_TURN_USER")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let pass = std::env::var("SUSI_TURN_PASS")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let realm_override = std::env::var("SUSI_TURN_REALM")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(TurnCreds {
        user,
        pass,
        realm_override,
    })
}

fn append_stun_attr(buf: &mut Vec<u8>, typ: u16, value: &[u8]) {
    buf.extend_from_slice(&typ.to_be_bytes());
    let len = u16::try_from(value.len()).unwrap_or(u16::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(value);
    let pad = (4 - (value.len() % 4)) % 4;
    buf.resize(buf.len() + pad, 0);
}

fn stun_allocate_header(txid: &[u8; 12], body_len: u16) -> [u8; STUN_HEADER_LEN] {
    let mut header = [0u8; STUN_HEADER_LEN];
    header[0..2].copy_from_slice(&TURN_ALLOCATE.to_be_bytes());
    header[2..4].copy_from_slice(&body_len.to_be_bytes());
    header[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    header[8..20].copy_from_slice(txid);
    header
}

/// RFC 5389 long-term credential key = MD5(username:realm:password).
fn turn_long_term_key(user: &str, realm: &str, pass: &str) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(user.as_bytes());
    h.update(b":");
    h.update(realm.as_bytes());
    h.update(b":");
    h.update(pass.as_bytes());
    h.finalize().into()
}

/// HMAC-SHA1 (STUN MESSAGE-INTEGRITY). Block size 64; 16-byte MD5 keys pad.
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; STUN_MI_LEN] {
    use sha1::{Digest, Sha1};
    const BLOCK: usize = 64;
    let mut keyed = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = Sha1::digest(key);
        keyed[..STUN_MI_LEN].copy_from_slice(&hashed);
    } else {
        keyed[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= keyed[i];
        opad[i] ^= keyed[i];
    }
    let mut inner = Sha1::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut mac = [0u8; STUN_MI_LEN];
    mac.copy_from_slice(&out);
    mac
}

fn requested_transport_attr() -> Vec<u8> {
    let mut body = Vec::new();
    append_stun_attr(
        &mut body,
        ATTR_REQUESTED_TRANSPORT,
        &[UDP_TRANSPORT, 0, 0, 0],
    );
    body
}

fn build_allocate_request(
    txid: &[u8; 12],
    integrity: Option<(&str, &str, &str, &[u8; 16])>,
) -> Vec<u8> {
    let mut body = requested_transport_attr();
    if let Some((user, realm, nonce, key)) = integrity {
        append_stun_attr(&mut body, ATTR_USERNAME, user.as_bytes());
        append_stun_attr(&mut body, ATTR_REALM, realm.as_bytes());
        append_stun_attr(&mut body, ATTR_NONCE, nonce.as_bytes());
        let mi_attr_len = 4 + STUN_MI_LEN;
        let total = u16::try_from(body.len() + mi_attr_len).unwrap_or(u16::MAX);
        let header = stun_allocate_header(txid, total);
        let mut mac_input = Vec::with_capacity(STUN_HEADER_LEN + body.len());
        mac_input.extend_from_slice(&header);
        mac_input.extend_from_slice(&body);
        let mac = hmac_sha1(key, &mac_input);
        append_stun_attr(&mut body, ATTR_MESSAGE_INTEGRITY, &mac);
        let mut msg = Vec::with_capacity(STUN_HEADER_LEN + body.len());
        msg.extend_from_slice(&header);
        msg.extend_from_slice(&body);
        msg
    } else {
        let total = u16::try_from(body.len()).unwrap_or(u16::MAX);
        let header = stun_allocate_header(txid, total);
        let mut msg = Vec::with_capacity(STUN_HEADER_LEN + body.len());
        msg.extend_from_slice(&header);
        msg.extend_from_slice(&body);
        msg
    }
}

struct StunAllocateError {
    code: u16,
    realm: Option<String>,
    nonce: Option<String>,
}

fn parse_stun_allocate_error(msg: &[u8], txid: &[u8; 12]) -> Option<StunAllocateError> {
    if be_u16(msg, 0)? != TURN_ALLOCATE_ERROR
        || msg.get(4..8)? != STUN_MAGIC_COOKIE.to_be_bytes()
        || msg.get(8..20)? != txid
    {
        return None;
    }
    let body_len = usize::from(be_u16(msg, 2)?);
    let body = msg.get(STUN_HEADER_LEN..STUN_HEADER_LEN + body_len)?;
    let mut code = None;
    let mut realm = None;
    let mut nonce = None;
    let mut at = 0;
    while at + 4 <= body.len() {
        let attr = be_u16(body, at)?;
        let len = usize::from(be_u16(body, at + 2)?);
        let value = body.get(at + 4..at + 4 + len)?;
        if attr == ATTR_ERROR_CODE && value.len() >= 4 {
            let class = u16::from(*value.get(2)?);
            let number = u16::from(*value.get(3)?);
            code = Some(class.saturating_mul(100).saturating_add(number));
        } else if attr == ATTR_REALM {
            realm = String::from_utf8(value.to_vec()).ok();
        } else if attr == ATTR_NONCE {
            nonce = String::from_utf8(value.to_vec()).ok();
        }
        at += 4 + len.div_ceil(4) * 4;
    }
    Some(StunAllocateError {
        code: code?,
        realm,
        nonce,
    })
}

fn recv_allocate_reply(
    socket: &UdpSocket,
    server: SocketAddr,
    txid: &[u8; 12],
) -> Result<Result<SocketAddr, StunAllocateError>, String> {
    let mut buf = [0u8; 576];
    loop {
        let (n, from) = socket
            .recv_from(&mut buf)
            .map_err(|e| format!("TURN recv from {server}: {e}"))?;
        if from != server {
            continue;
        }
        let msg = buf.get(..n).unwrap_or_default();
        if let Some(addr) = parse_stun_success(
            msg,
            txid,
            TURN_ALLOCATE_SUCCESS,
            ATTR_XOR_RELAYED_ADDRESS,
            ATTR_XOR_RELAYED_ADDRESS,
        ) {
            return Ok(Ok(addr));
        }
        if let Some(err) = parse_stun_allocate_error(msg, txid) {
            return Ok(Err(err));
        }
        return Err(format!("malformed TURN Allocate reply from {server}"));
    }
}

/// RFC 5766 Allocate: unauthenticated first, then MESSAGE-INTEGRITY on 401/438.
fn turn_allocate(
    server: SocketAddr,
    timeout: Duration,
    creds: Option<TurnCreds>,
) -> Result<SocketAddr, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    };
    let socket = UdpSocket::bind(bind).map_err(|e| format!("bind TURN socket: {e}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set TURN timeout: {e}"))?;

    let mut realm = creds
        .as_ref()
        .and_then(|c| c.realm_override.clone())
        .unwrap_or_default();
    let mut nonce = String::new();
    let mut use_integrity = false;
    for attempt in 0..3 {
        let mut txid = [0u8; 12];
        getrandom::fill(&mut txid).map_err(|e| format!("TURN transaction id: {e}"))?;
        let request = if use_integrity {
            let Some(c) = creds.as_ref() else {
                return Err(
                    "TURN 401: set SUSI_TURN_USER and SUSI_TURN_PASS (or SUSI_TURN_RELAY)"
                        .to_string(),
                );
            };
            if realm.is_empty() || nonce.is_empty() {
                return Err("TURN 401 missing REALM/NONCE".to_string());
            }
            let key = turn_long_term_key(&c.user, &realm, &c.pass);
            build_allocate_request(&txid, Some((&c.user, &realm, &nonce, &key)))
        } else {
            build_allocate_request(&txid, None)
        };
        socket
            .send_to(&request, server)
            .map_err(|e| format!("TURN send to {server}: {e}"))?;
        match recv_allocate_reply(&socket, server, &txid)? {
            Ok(addr) => return Ok(addr),
            Err(err) => {
                if err.code == 401 || err.code == 438 {
                    if let Some(r) = err.realm.filter(|s| !s.is_empty()) {
                        realm = r;
                    }
                    if let Some(n) = err.nonce.filter(|s| !s.is_empty()) {
                        nonce = n;
                    }
                    if creds.is_none() {
                        return Err(format!(
                            "TURN {code} at {server}: set SUSI_TURN_USER and SUSI_TURN_PASS (or SUSI_TURN_RELAY)",
                            code = err.code
                        ));
                    }
                    use_integrity = true;
                    continue;
                }
                return Err(format!(
                    "TURN Allocate error {code} at {server} (attempt {attempt})",
                    code = err.code
                ));
            }
        }
    }
    Err(format!("TURN Allocate exhausted retries at {server}"))
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
