//! Connection setup for the outbound client: DNS and TCP under the request
//! deadline, the HTTP CONNECT proxy tunnel, and verified TLS.
//!
//! TLS is the platform stack behind `susi-vendor-tokio-rustls` (Schannel on
//! Windows, platform OpenSSL on Linux, Security.framework on Apple), used
//! synchronously: certificate chain, expiry and hostname verification stay
//! the native-tls defaults with the platform trust store, TLS 1.2 minimum.
//! Nothing in this module can disable verification.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio_rustls::native_tls;

use crate::wire::Target;

/// Connect attempts never wait longer than this, whatever the budget.
const CONNECT_CAP: Duration = Duration::from_secs(15);

pub(crate) fn timeout_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "timeout: the request exceeded its total time budget",
    )
}

pub(crate) fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(timeout_error())
    } else {
        Ok(left)
    }
}

/// A TCP socket whose every read and write is bounded by the request
/// deadline: one budget covers connect, TLS, headers and the whole body.
#[derive(Debug)]
pub(crate) struct TimedSocket {
    tcp: TcpStream,
    deadline: Instant,
}

fn map_timeout(error: io::Error) -> io::Error {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        timeout_error()
    } else {
        error
    }
}

impl Read for TimedSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = remaining(self.deadline)?;
        self.tcp.set_read_timeout(Some(left))?;
        self.tcp.read(buf).map_err(map_timeout)
    }
}

impl Write for TimedSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = remaining(self.deadline)?;
        self.tcp.set_write_timeout(Some(left))?;
        self.tcp.write(buf).map_err(map_timeout)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

/// A connected byte stream: plain, TLS, or TLS through a tunnel.
pub(crate) trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

pub(crate) type Conn = Box<dyn Stream>;

/// Proxy protocols the environment may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyKind {
    Http,
    Https,
    /// SOCKS is not implemented: as before (ureq without its SOCKS
    /// feature), a SOCKS proxy from the environment is not used.
    Socks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub credentials: Option<(String, String)>,
    no_proxy: Vec<NoProxyEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NoProxyEntry {
    All,
    Exact(String),
    Prefix(String),
    Suffix(String),
}

impl NoProxyEntry {
    fn parse(entry: &str) -> Option<Self> {
        let entry = entry.trim();
        if entry.is_empty() {
            return None;
        }
        let lower = entry.to_ascii_lowercase();
        Some(if lower == "*" {
            Self::All
        } else if let Some(rest) = lower.strip_prefix('*') {
            Self::Suffix(rest.to_string())
        } else if lower.starts_with('.') {
            Self::Suffix(lower)
        } else if let Some(rest) = lower.strip_suffix('*') {
            Self::Prefix(rest.to_string())
        } else if lower.ends_with('.') {
            Self::Prefix(lower)
        } else {
            Self::Exact(lower)
        })
    }

    fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            Self::All => true,
            Self::Exact(pattern) => *pattern == host,
            Self::Prefix(prefix) => host.starts_with(prefix.as_str()),
            Self::Suffix(suffix) => host.ends_with(suffix.as_str()),
        }
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Proxy {
    /// Parse a proxy URL; a bare `host:port` means `http://`.
    pub fn parse(value: &str, no_proxy: Option<&str>) -> Option<Self> {
        let value = value.trim();
        let (scheme, rest) = value.split_once("://").unwrap_or(("http", value));
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "http" => ProxyKind::Http,
            "https" => ProxyKind::Https,
            "socks4" | "socks4a" | "socks5" | "socks5h" => ProxyKind::Socks,
            _ => return None,
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((user, host)) => (Some(user), host),
            None => (None, authority),
        };
        let credentials = match userinfo {
            Some(info) => {
                let (user, pass) = info.split_once(':').unwrap_or((info, ""));
                Some((percent_decode(user)?, percent_decode(pass)?))
            }
            None => None,
        };
        let default_port = match kind {
            ProxyKind::Http => 80,
            ProxyKind::Https => 443,
            ProxyKind::Socks => 1080,
        };
        let probe = Target::parse(&format!("http://{hostport}/")).ok()?;
        let port = if probe.explicit_port {
            probe.port
        } else {
            default_port
        };
        Some(Self {
            kind,
            host: probe.host,
            port,
            credentials,
            no_proxy: no_proxy
                .map(|list| list.split(',').filter_map(NoProxyEntry::parse).collect())
                .unwrap_or_default(),
        })
    }

    /// The proxy the environment configures, read the way ureq read it:
    /// the first of `ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY` (each upper
    /// then lower case) that parses, for every target scheme, with
    /// `NO_PROXY` / `no_proxy` exclusions.
    pub fn from_env() -> Option<Self> {
        let no_proxy = ["NO_PROXY", "no_proxy"]
            .iter()
            .find_map(|name| std::env::var(name).ok());
        [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find_map(|value| Self::parse(&value, no_proxy.as_deref()))
    }

    fn bypasses(&self, host: &str) -> bool {
        self.no_proxy.iter().any(|entry| entry.matches(host))
    }
}

/// Resolve `host:port` within the deadline. getaddrinfo cannot be
/// cancelled, so a hung lookup is left on its own thread.
fn resolve(host: &str, port: u16, deadline: Instant) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let left = remaining(deadline).map_err(|e| e.to_string())?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let lookup = host.to_string();
    std::thread::Builder::new()
        .name("susi-http-dns".into())
        .spawn(move || {
            let _ = sender.send((lookup.as_str(), port).to_socket_addrs());
        })
        .map_err(|e| format!("dns: cannot start the resolver thread: {e}"))?;
    match receiver.recv_timeout(left) {
        Ok(Ok(addrs)) => {
            let addrs: Vec<SocketAddr> = addrs.collect();
            if addrs.is_empty() {
                Err(format!("dns: {host} resolved to no address"))
            } else {
                Ok(addrs)
            }
        }
        Ok(Err(error)) => Err(format!("dns: failed to resolve {host}: {error}")),
        Err(_) => Err(format!(
            "timeout: resolving {host} exceeded the request budget"
        )),
    }
}

fn tcp_connect(host: &str, port: u16, deadline: Instant) -> Result<TimedSocket, String> {
    let addrs = resolve(host, port, deadline)?;
    let mut last = None;
    for addr in addrs {
        let left = remaining(deadline).map_err(|e| e.to_string())?;
        match TcpStream::connect_timeout(&addr, left.min(CONNECT_CAP)) {
            Ok(tcp) => {
                let _ = tcp.set_nodelay(true);
                return Ok(TimedSocket { tcp, deadline });
            }
            Err(error) => last = Some(map_timeout(error)),
        }
    }
    Err(match last {
        Some(error) => format!("connect to {host}:{port}: {error}"),
        None => format!("connect to {host}:{port}: no address"),
    })
}

fn platform_connector() -> Result<&'static native_tls::TlsConnector, String> {
    static CONNECTOR: OnceLock<Result<native_tls::TlsConnector, String>> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            native_tls::TlsConnector::new().map_err(|e| format!("tls: platform setup: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Wrap `stream` in verified TLS for `host`.
fn tls<S: Read + Write + Send + 'static>(
    connector: &native_tls::TlsConnector,
    host: &str,
    stream: S,
    deadline: Instant,
) -> Result<Conn, String> {
    match connector.connect(host, stream) {
        Ok(tls) => Ok(Box::new(tls)),
        Err(native_tls::HandshakeError::Failure(error)) => {
            if remaining(deadline).is_err() {
                Err(timeout_error().to_string())
            } else {
                Err(format!("tls: handshake with {host} failed: {error}"))
            }
        }
        Err(native_tls::HandshakeError::WouldBlock(_)) => Err(timeout_error().to_string()),
    }
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(TABLE[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Open a CONNECT tunnel to `target` through `proxy`.
fn tunnel(
    proxy: &Proxy,
    target: &Target,
    connector: &native_tls::TlsConnector,
    deadline: Instant,
) -> Result<Conn, String> {
    let socket = tcp_connect(&proxy.host, proxy.port, deadline)?;
    let mut stream: Conn = match proxy.kind {
        ProxyKind::Https => tls(connector, &proxy.host, socket, deadline)?,
        ProxyKind::Http | ProxyKind::Socks => Box::new(socket),
    };
    let authority = target.connect_authority();
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {}\r\nProxy-Connection: Keep-Alive\r\n",
        crate::client::USER_AGENT
    );
    if let Some((user, pass)) = &proxy.credentials {
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(format!("{user}:{pass}").as_bytes())
        ));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("proxy {}:{}: {e}", proxy.host, proxy.port))?;
    // Read the reply one byte at a time: nothing past its head may be
    // consumed, because the tunnel's first bytes belong to the target.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > crate::wire::MAX_HEAD_BYTES {
            return Err("proxy server did not respond: CONNECT reply too large".into());
        }
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => return Err("proxy server did not respond".into()),
            Ok(_) => head.push(byte[0]),
            Err(error) => return Err(format!("proxy {}:{}: {error}", proxy.host, proxy.port)),
        }
    }
    let parsed = crate::wire::read_head(&mut io::Cursor::new(head))
        .map_err(|e| format!("proxy server sent an invalid CONNECT reply: {e}"))?;
    if parsed.status != 200 {
        return Err(format!("proxy server responded {}", parsed.status));
    }
    Ok(stream)
}

/// Connect to `target`, through `proxy` unless it is absent, SOCKS or
/// excluded by `NO_PROXY`, with TLS for `https://`.
pub(crate) fn open(
    target: &Target,
    proxy: Option<&Proxy>,
    connector: Option<&native_tls::TlsConnector>,
    deadline: Instant,
) -> Result<Conn, String> {
    let connector = match connector {
        Some(connector) => connector,
        None => platform_connector()?,
    };
    let proxy = proxy.filter(|p| {
        matches!(p.kind, ProxyKind::Http | ProxyKind::Https)
            && !p.bypasses(&target.host_for_authority())
    });
    let stream: Conn = match proxy {
        Some(proxy) => tunnel(proxy, target, connector, deadline)?,
        None => Box::new(tcp_connect(&target.host, target.port, deadline)?),
    };
    if target.is_https() {
        tls(connector, &target.host, stream, deadline)
    } else {
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls_parse_like_the_environment_names_them() {
        let p = Proxy::parse("http://user:p%40ss@proxy.example:3128", None).unwrap();
        assert_eq!(p.kind, ProxyKind::Http);
        assert_eq!((p.host.as_str(), p.port), ("proxy.example", 3128));
        assert_eq!(p.credentials, Some(("user".into(), "p@ss".into())));
        let bare = Proxy::parse("localhost:9999", None).unwrap();
        assert_eq!((bare.kind, bare.port), (ProxyKind::Http, 9999));
        assert_eq!(Proxy::parse("https://p.example", None).unwrap().port, 443);
        assert_eq!(
            Proxy::parse("socks5://p.example", None).unwrap().kind,
            ProxyKind::Socks
        );
        assert!(Proxy::parse("fakeproto://localhost", None).is_none());
        assert!(Proxy::parse("", None).is_none());
    }

    #[test]
    fn no_proxy_entries_match_exact_prefix_suffix_and_all() {
        let p = Proxy::parse(
            "http://p:1",
            Some("localhost, .internal.com,*.corp,10.0.*,API.example"),
        )
        .unwrap();
        for host in [
            "localhost",
            "a.internal.com",
            "x.corp",
            "10.0.0.7",
            "api.example",
        ] {
            assert!(p.bypasses(host), "{host}");
        }
        for host in ["cloud.example", "internal.com.evil", "10.1.0.1"] {
            assert!(!p.bypasses(host), "{host}");
        }
        assert!(Proxy::parse("http://p:1", Some("*"))
            .unwrap()
            .bypasses("any"));
        assert!(!Proxy::parse("http://p:1", Some(""))
            .unwrap()
            .bypasses("any"));
    }

    #[test]
    fn basic_credentials_encode_as_standard_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:p@ss"), "dXNlcjpwQHNz");
    }

    #[test]
    fn an_expired_deadline_fails_before_any_socket_io() {
        let past = Instant::now() - Duration::from_millis(1);
        let error = tcp_connect("127.0.0.1", 9, past).err().unwrap();
        assert!(error.contains("timeout"), "{error}");
    }
}
