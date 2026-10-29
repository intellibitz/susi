//! The one loopback HTTP/1.0 client every SUSI service client uses.
//!
//! Leaf-service clients (paths, config, sandbox, native, error events, the
//! CLI's GEMI calls) talk to `127.0.0.1:<port>` with tiny JSON requests.
//! Each used to hand-roll the same connect-with-timeout / raw request /
//! read-to-end / status-line / body split; this module is that code once,
//! std-only, so foundation crates stay dependency-free.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

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

/// One request: `method path` with an optional JSON body. `None` on transport
/// failure, response overflow, or an unparseable response.
#[must_use]
pub fn request(ep: &Endpoint, method: &str, path: &str, json: Option<&str>) -> Option<Response> {
    exchange(connect(ep).ok()?, ep, method, path, json)
}

/// [`request`] for a client that has a local fallback when its service is
/// unreachable (paths, config, error events, sandbox state).
///
/// A connect to a closed loopback port is refused at once on Linux, but on
/// Windows the stack retries the SYN and only refuses after about two
/// seconds, so every such call costs the client's whole connect timeout.
/// While the substrate is down — every CLI command before `susi start`, and
/// the daemon's own boot until it brings its leaf services up — clients
/// make thousands of these calls, and the boot that should take seconds
/// took tens of minutes. A refused or timed-out connect therefore puts the
/// port in a short, doubling [`Backoff`]; until it runs out the call
/// returns `None` without touching the network and the caller takes its
/// local path, exactly as it would after a failed connect. A connect that
/// succeeds clears the backoff.
#[must_use]
pub fn request_with_fallback(
    ep: &Endpoint,
    method: &str,
    path: &str,
    json: Option<&str>,
) -> Option<Response> {
    let now = Instant::now();
    if unreachable_ports()
        .lock()
        .is_ok_and(|b| b.skips(ep.port, now))
    {
        return None;
    }
    match connect(ep) {
        Ok(stream) => {
            if let Ok(mut b) = unreachable_ports().lock() {
                b.reachable(ep.port);
            }
            exchange(stream, ep, method, path, json)
        }
        Err(_) => {
            if let Ok(mut b) = unreachable_ports().lock() {
                b.unreachable(ep.port, Instant::now());
            }
            None
        }
    }
}

/// True while `port` is in its unreachable backoff — what
/// [`request_with_fallback`] would skip right now.
#[must_use]
pub fn backed_off(port: u16) -> bool {
    unreachable_ports()
        .lock()
        .is_ok_and(|b| b.skips(port, Instant::now()))
}

fn unreachable_ports() -> &'static Mutex<Backoff> {
    static PORTS: OnceLock<Mutex<Backoff>> = OnceLock::new();
    PORTS.get_or_init(|| Mutex::new(Backoff::default()))
}

/// First backoff after a failed connect.
pub const BACKOFF_FIRST: Duration = Duration::from_millis(500);
/// Ceiling of the doubling backoff — also the longest a client keeps using
/// its local fallback after the service has come back.
pub const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// Per-port memory of failed connects: each consecutive failure doubles the
/// window (from [`BACKOFF_FIRST`] up to [`BACKOFF_MAX`]) during which the
/// port is not dialled again.
#[derive(Debug, Default)]
pub struct Backoff {
    ports: HashMap<u16, (Instant, u32)>,
}

impl Backoff {
    /// Whether a call to `port` at `now` is skipped.
    #[must_use]
    pub fn skips(&self, port: u16, now: Instant) -> bool {
        self.ports.get(&port).is_some_and(|(until, _)| now < *until)
    }

    /// Record a failed connect to `port` at `now`.
    pub fn unreachable(&mut self, port: u16, now: Instant) {
        let streak = self
            .ports
            .get(&port)
            .map_or(0, |(_, s)| *s)
            .saturating_add(1);
        let factor = 1u32 << streak.saturating_sub(1).min(16);
        let window = BACKOFF_FIRST.saturating_mul(factor).min(BACKOFF_MAX);
        self.ports.insert(port, (now + window, streak));
    }

    /// Record a successful connect to `port`.
    pub fn reachable(&mut self, port: u16) {
        self.ports.remove(&port);
    }
}

fn connect(ep: &Endpoint) -> std::io::Result<TcpStream> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), ep.port);
    TcpStream::connect_timeout(&addr, ep.timeout)
}

fn exchange(
    mut stream: TcpStream,
    ep: &Endpoint,
    method: &str,
    path: &str,
    json: Option<&str>,
) -> Option<Response> {
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
    stream
        .take(MAX_RESPONSE_BYTES.saturating_add(1))
        .read_to_string(&mut raw)
        .ok()?;
    if u64::try_from(raw.len()).ok()? > MAX_RESPONSE_BYTES {
        return None;
    }
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
    use super::{Backoff, Response, BACKOFF_FIRST, BACKOFF_MAX};
    use std::time::{Duration, Instant};

    #[test]
    fn a_failed_connect_backs_the_port_off_and_doubles_to_a_ceiling() {
        let mut b = Backoff::default();
        let t0 = Instant::now();
        assert!(!b.skips(18080, t0), "an unknown port is dialled");
        b.unreachable(18080, t0);
        assert!(b.skips(18080, t0));
        assert!(b.skips(18080, t0 + BACKOFF_FIRST - Duration::from_millis(1)));
        assert!(!b.skips(18080, t0 + BACKOFF_FIRST), "the window runs out");
        // Other ports are untouched.
        assert!(!b.skips(18082, t0));
        // Consecutive failures double the window, up to the ceiling.
        let t1 = t0 + BACKOFF_FIRST;
        b.unreachable(18080, t1);
        assert!(b.skips(18080, t1 + BACKOFF_FIRST));
        assert!(!b.skips(18080, t1 + BACKOFF_FIRST * 2));
        for _ in 0..40 {
            b.unreachable(18080, t1);
        }
        assert!(b.skips(18080, t1 + BACKOFF_MAX - Duration::from_millis(1)));
        assert!(!b.skips(18080, t1 + BACKOFF_MAX));
        // A successful connect clears it at once.
        b.reachable(18080);
        assert!(!b.skips(18080, t1));
    }

    /// The fallback client never pays a second connect timeout to a port it
    /// just found closed: on Windows each one costs the whole timeout (the
    /// stack retries the SYN for ~2 s before refusing), which is what held a
    /// daemon boot for tens of minutes.
    #[test]
    fn request_with_fallback_skips_a_port_it_just_found_closed() {
        // A port that was just free: bind, read it, release it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .unwrap();
        let ep = super::Endpoint {
            port,
            timeout: Duration::from_millis(200),
            auth: super::Auth::None,
        };
        assert!(super::request_with_fallback(&ep, "GET", "/x", None).is_none());
        assert!(
            super::backed_off(port),
            "the failed connect starts a backoff"
        );
        let started = Instant::now();
        assert!(super::request_with_fallback(&ep, "GET", "/x", None).is_none());
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "a backed-off port is not dialled again: {:?}",
            started.elapsed()
        );
        // The plain client keeps dialling — it has no local fallback.
        assert!(super::request(&ep, "GET", "/x", None).is_none());
    }

    #[test]
    fn parses_status_and_body() {
        let r = Response::parse("HTTP/1.1 204 No Content\r\nX: y\r\n\r\n").unwrap();
        assert_eq!((r.status, r.is_success(), r.body.as_str()), (204, true, ""));
        let r = Response::parse("HTTP/1.0 401 Unauthorized\r\n\r\nno").unwrap();
        assert_eq!((r.status, r.ok_body()), (401, None));
        assert!(Response::parse("garbage").is_none());
    }
}
