//! The one loopback HTTP/1.0 client every SUSI service client uses.
//!
//! Leaf-service clients (paths, config, sandbox, native, error events, the
//! CLI's GEMI calls) talk to `127.0.0.1:<port>` with tiny JSON requests.
//! Each used to hand-roll the same connect-with-timeout / raw request /
//! read-to-end / status-line / body split; this module is that code once,
//! std-only, so foundation crates stay dependency-free.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// A parsed loopback response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code from the status line (0 when it is not `HTTP/1.x NNN`).
    pub status: u16,
    pub body: String,
}

impl Response {
    /// Whether the status is 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body when the status is 2xx.
    #[must_use]
    pub fn ok_body(&self) -> Option<&str> {
        self.is_success().then_some(self.body.as_str())
    }

    fn parse(raw: &str) -> Option<Self> {
        let (head, body) = raw.split_once("\r\n\r\n")?;
        let status = head
            .lines()
            .next()
            .filter(|line| line.starts_with("HTTP/1."))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        Some(Self {
            status,
            body: body.to_string(),
        })
    }
}

/// Which bearer, if any, to attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// No `Authorization` header.
    None,
    /// The host token file (`<config_dir>/api_token`) when seeded.
    HostToken,
}

/// Where a loopback call goes: `127.0.0.1:port`, one `timeout` applied to
/// connect, write and read each, and the bearer policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub timeout: Duration,
    pub auth: Auth,
}

/// One request: `method path` with an optional JSON body. `None` on any
/// transport failure or an unparseable response.
#[must_use]
pub fn request(ep: &Endpoint, method: &str, path: &str, json: Option<&str>) -> Option<Response> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ep.port);
    let mut stream = TcpStream::connect_timeout(&addr, ep.timeout).ok()?;
    let _ = stream.set_read_timeout(Some(ep.timeout));
    let _ = stream.set_write_timeout(Some(ep.timeout));
    let bearer = match ep.auth {
        Auth::HostToken => crate::host_token()
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default(),
        Auth::None => String::new(),
    };
    let body = json.map_or_else(String::new, |b| {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
            b.len()
        )
    });
    let head_end = if json.is_some() { "" } else { "\r\n" };
    let req = format!("{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n{bearer}{body}{head_end}");
    stream.write_all(req.as_bytes()).ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    Response::parse(&raw)
}

/// Port of a leaf service client: `env_key` (e.g. `SUSI_CONFIG_PORT`) wins,
/// else `default` shifted by the instance offset — a dev instance
/// (`SUSI_PORT_OFFSET=100`) reaches its own services, not the release
/// instance's.
#[must_use]
pub fn service_port(env_key: &str, default: u16) -> u16 {
    std::env::var(env_key)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or_else(|| crate::ports::effective(default))
}

/// Explicit local env config (`SUSI_XDG`, `XDG_*_HOME`): clients must then
/// resolve locally — a swapped HOME in tests (or a second instance) must
/// not read or write the host substrate through its services.
#[must_use]
pub fn local_env_override() -> bool {
    [
        "SUSI_XDG",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
    ]
    .iter()
    .any(|v| std::env::var_os(v).is_some())
}

#[cfg(test)]
mod tests {
    use super::Response;

    #[test]
    fn parses_status_and_body() {
        let r = Response::parse("HTTP/1.1 204 No Content\r\nX: y\r\n\r\n").unwrap();
        assert_eq!((r.status, r.is_success(), r.body.as_str()), (204, true, ""));
        let r = Response::parse("HTTP/1.0 401 Unauthorized\r\n\r\nno").unwrap();
        assert_eq!((r.status, r.ok_body()), (401, None));
        assert!(Response::parse("garbage").is_none());
    }
}
