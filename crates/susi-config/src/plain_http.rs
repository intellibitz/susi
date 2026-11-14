//! Blocking HTTP/1.1 client over `std::net::TcpStream`, plaintext
//! `http://` only.
//!
//! This is the transport of `susi_core::mcp_client`, the peer
//! channel between Susi hosts. It keeps `susi-core` off the async runtime and
//! the third-party HTTP stack: one request per connection
//! (`Connection: close`), no pooling, no TLS, no proxy.
//!
//! Behaviour the peer channel relies on:
//!
//! - One budget (`timeout_secs`, at least one second) covers resolving,
//!   connecting, sending and reading the whole response, including the body
//!   the caller reads later through [`Response::into_bytes`]. Connecting to
//!   one address waits at most 15 seconds of it.
//! - HTTP error statuses are returned as responses, never as `Err`; callers
//!   check [`Response::status`].
//! - Redirects are never followed: a 3xx is returned to the caller like any
//!   other status (the previous transport ran this channel with
//!   `max_redirects = 0`, which behaves the same way).
//! - `https://` and every other scheme are refused before any connection, by
//!   name: this client never speaks TLS, so nothing it sends is protected by
//!   the scheme. The peer channel authenticates by member signature and seals
//!   bodies itself (`mcp_client`).
//! - The connection is direct. Proxy variables (`HTTP_PROXY`, `ALL_PROXY`)
//!   are not consulted: the peer channel goes to a cluster member on this
//!   machine or its local network, and a proxy would be an extra party on it.
//! - Response bodies are decoded from `Content-Length`, chunked transfer
//!   coding, or read until the server closes the connection. Interim `1xx`
//!   responses are skipped. Content codings are not requested, so the body
//!   arrives as sent.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Cap on a response head (status line plus headers) and on one chunk-size
/// or trailer line, so a peer that never sends a newline cannot grow memory
/// without limit.
const MAX_HEAD_BYTES: u64 = 64 * 1024;

/// Longest wait for one TCP connect, within the request budget.
const MAX_CONNECT: Duration = Duration::from_secs(15);

/// Interim (`1xx`) responses tolerated before the final one.
const MAX_INTERIM_RESPONSES: usize = 8;

/// The parts of an `http://` URL a request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// Host without brackets (`127.0.0.1`, `::1`, `peer.lan`).
    host: String,
    port: u16,
    /// `Host` header value: the authority as written in the URL.
    authority: String,
    /// Path plus query, never empty.
    path: String,
}

fn parse_url(url: &str) -> Result<Target, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url}: not an absolute http:// URL"))?;
    if scheme.eq_ignore_ascii_case("https") {
        return Err(format!(
            "{url}: https:// is not supported by this client; it speaks plaintext http:// only"
        ));
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return Err(format!("{url}: unsupported URL scheme `{scheme}`"));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() {
        return Err(format!("{url}: no host"));
    }
    if authority.contains('@') {
        return Err(format!("{url}: credentials in the URL are not supported"));
    }
    let tail = tail.split('#').next().unwrap_or_default();
    let path = if tail.starts_with('/') {
        tail.to_string()
    } else {
        format!("/{tail}")
    };
    let (host, port) = match authority.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed
                .split_once(']')
                .ok_or_else(|| format!("{url}: unterminated IPv6 literal"))?;
            let port = match after {
                "" => None,
                other => Some(
                    other
                        .strip_prefix(':')
                        .ok_or_else(|| format!("{url}: malformed authority"))?,
                ),
            };
            (host, port)
        }
        None => match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    let port = match port {
        None => 80,
        Some(port) => port
            .parse::<u16>()
            .map_err(|_| format!("{url}: invalid port `{port}`"))?,
    };
    Ok(Target {
        host: host.to_string(),
        port,
        authority: authority.to_string(),
        path,
    })
}

/// One completed request: status, headers, and a body still on the wire.
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Header names lower-cased, in arrival order.
    headers: Vec<(String, String)>,
    body: Body,
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.body.read(buf)
    }
}

impl Response {
    /// First header value named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Read the whole body, refusing one longer than `max` bytes. One extra
    /// byte is read to detect overflow. The request budget still applies.
    pub fn into_bytes(self, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.body
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response body exceeds configured limit",
            ));
        }
        Ok(bytes)
    }
}

/// A byte stream one request runs over: a TCP connection, or a sealed
/// peer-channel session (`susi_config::peer_channel_link`), whose bytes are
/// the same HTTP/1.1 exchange carried end to end inside the channel.
pub trait Stream: Read + Write + Send {
    /// Bound the next read to `left`.
    fn read_within(&mut self, left: Duration) -> io::Result<()>;
    /// Bound the next write to `left`.
    fn write_within(&mut self, left: Duration) -> io::Result<()>;
}

impl Stream for TcpStream {
    fn read_within(&mut self, left: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(left))
    }

    fn write_within(&mut self, left: Duration) -> io::Result<()> {
        self.set_write_timeout(Some(left))
    }
}

impl Stream for crate::peer_channel_link::ChannelStream {
    fn read_within(&mut self, left: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(left));
        Ok(())
    }

    /// A channel write is bounded by its link's socket write timeout.
    fn write_within(&mut self, _left: Duration) -> io::Result<()> {
        Ok(())
    }
}

/// Validate the method, URL and headers of a request.
fn check_request(method: &str, url: &str, headers: &[(&str, &str)]) -> Result<Target, String> {
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("unsupported HTTP method `{method}`"));
    }
    let target = parse_url(url)?;
    for (name, value) in headers {
        let bad = |s: &str| s.bytes().any(|b| b == b'\r' || b == b'\n');
        if name.is_empty() || bad(name) || name.contains(':') || bad(value) {
            return Err(format!("invalid request header `{name}`"));
        }
    }
    Ok(target)
}

/// Send `method url` with `headers` and an optional `body`, and return once
/// the response head has arrived. `timeout_secs` (at least one) bounds the
/// whole exchange, body reads included. See the module notes for scheme,
/// redirect and proxy behaviour.
pub fn call(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout_secs: u64,
) -> Result<Response, String> {
    call_within(
        method,
        url,
        headers,
        body,
        Duration::from_secs(timeout_secs.max(1)),
    )
}

pub fn call_within(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    budget: Duration,
) -> Result<Response, String> {
    let target = check_request(method, url, headers)?;
    let deadline = Instant::now() + budget.max(Duration::from_millis(1));
    let stream = connect(&target, deadline)?;
    exchange(Box::new(stream), &target, method, headers, body, deadline)
}

/// [`call`] over an already open `stream` instead of a new TCP connection
/// to the URL's host: the URL supplies the request target and `Host`, and
/// the same budget bounds the exchange.
#[allow(clippy::too_many_arguments)] // `call`'s flat request parts plus the stream
pub fn call_over(
    stream: Box<dyn Stream>,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout_secs: u64,
) -> Result<Response, String> {
    call_over_within(
        stream,
        method,
        url,
        headers,
        body,
        Duration::from_secs(timeout_secs.max(1)),
    )
}

#[allow(clippy::too_many_arguments)] // `call`'s flat request parts plus the stream
pub fn call_over_within(
    stream: Box<dyn Stream>,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    budget: Duration,
) -> Result<Response, String> {
    let target = check_request(method, url, headers)?;
    let deadline = Instant::now() + budget.max(Duration::from_millis(1));
    exchange(stream, &target, method, headers, body, deadline)
}

/// Write one request on `stream` and read the response head.
#[allow(clippy::too_many_arguments)] // the request's flat parts, its stream and deadline
fn exchange(
    stream: Box<dyn Stream>,
    target: &Target,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    deadline: Instant,
) -> Result<Response, String> {
    let mut conn = Conn { stream, deadline };

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        target.path, target.authority
    );
    if let Some(body) = body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    conn.write_all(head.as_bytes())
        .and_then(|()| conn.write_all(body.unwrap_or_default()))
        .and_then(|()| conn.flush())
        .map_err(|e| format!("send to {}: {e}", target.authority))?;

    read_response(BufReader::new(conn), method)
        .map_err(|e| format!("read from {}: {e}", target.authority))
}

/// Connect to the first address of `target` that accepts within the budget.
fn connect(target: &Target, deadline: Instant) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {}: {e}", target.authority))?
        .collect();
    let mut last = format!("no address for {}", target.authority);
    for addr in addrs {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "connect {}: no connection within the time budget",
                target.authority
            ));
        }
        match TcpStream::connect_timeout(&addr, left.min(MAX_CONNECT)) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = format!("connect {} ({addr}): {e}", target.authority),
        }
    }
    Err(last)
}

/// The socket a request uses: every read and write waits at most until the
/// request deadline.
struct Conn {
    stream: Box<dyn Stream>,
    deadline: Instant,
}

impl Conn {
    /// Time left, or the budget error once it is spent.
    fn left(&self) -> io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err(over_budget())
        } else {
            Ok(left)
        }
    }

    /// A socket timeout that ends at the deadline is the budget running
    /// out, whichever error kind the OS reports for it. The socket was armed
    /// with exactly the time left, so a timeout reported up to one coarse
    /// timer interval early still is that budget: Windows socket timers fire
    /// a few milliseconds before the deadline by this process's clock, and
    /// such a timeout surfaced as a raw "connected party did not properly
    /// respond" error. A timed-out socket is not retried (Windows leaves its
    /// state indeterminate).
    fn budget_error(&self, e: io::Error) -> io::Error {
        let timed_out = matches!(
            e.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        );
        if timed_out && Instant::now() + TIMER_SLACK >= self.deadline {
            over_budget()
        } else {
            e
        }
    }
}

/// How early an OS socket timeout may fire relative to `Instant` (a few
/// Windows timer ticks of 15.6 ms).
const TIMER_SLACK: Duration = Duration::from_millis(50);

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.stream.read_within(left)?;
        self.stream.read(buf).map_err(|e| self.budget_error(e))
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.stream.write_within(left)?;
        self.stream.write(buf).map_err(|e| self.budget_error(e))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn over_budget() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "response did not complete within its time budget",
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Read one line ending in `\n` into `line`, refusing more than `limit`
/// bytes without a newline.
fn read_line_capped<R: BufRead>(reader: &mut R, line: &mut Vec<u8>, limit: u64) -> io::Result<()> {
    reader.take(limit).read_until(b'\n', line)?;
    if !line.ends_with(b"\n") && line.len() as u64 >= limit {
        return Err(invalid("response line too long"));
    }
    Ok(())
}

/// One response head: status and lower-cased headers.
type Head = (u16, Vec<(String, String)>);

/// Read one response head.
fn read_head<R: BufRead>(reader: &mut R) -> io::Result<Head> {
    let mut lines = Vec::new();
    let mut used = 0;
    loop {
        let mut line = Vec::new();
        read_line_capped(reader, &mut line, MAX_HEAD_BYTES - used)?;
        used += line.len() as u64;
        if line.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before a complete response head",
            ));
        }
        if !line.ends_with(b"\n") {
            return Err(invalid("response has no header terminator"));
        }
        let line = String::from_utf8_lossy(&line)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut lines = lines.into_iter();
    let status_line = lines
        .next()
        .ok_or_else(|| invalid("response has no status line"))?;
    let mut parts = status_line.split_whitespace();
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err(invalid("response is not HTTP/1.x"));
    }
    let status = parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (100..600).contains(code))
        .ok_or_else(|| invalid("response has no valid status code"))?;
    let headers = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Ok((status, headers))
}

/// Read the final response head (skipping interim `1xx` heads) and frame
/// the body that follows. `method` matters only for `HEAD`.
fn read_response(mut reader: BufReader<Conn>, method: &str) -> io::Result<Response> {
    let mut interim = 0;
    let (status, headers) = loop {
        let (status, headers) = read_head(&mut reader)?;
        // 101 switches protocols: there is no further HTTP/1.1 head to read.
        if (100..200).contains(&status) && status != 101 {
            interim += 1;
            if interim > MAX_INTERIM_RESPONSES {
                return Err(invalid("too many interim responses"));
            }
            continue;
        }
        break (status, headers);
    };
    let field = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let chunked = field("transfer-encoding").is_some_and(|v| {
        v.rsplit(',')
            .next()
            .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"))
    });
    let no_body = method.eq_ignore_ascii_case("HEAD")
        || (100..200).contains(&status)
        || status == 204
        || status == 304;
    let framing = if no_body {
        Framing::Length(0)
    } else if chunked {
        Framing::Chunked {
            remaining: 0,
            done: false,
        }
    } else if let Some(length) = field("content-length") {
        Framing::Length(
            length
                .parse::<u64>()
                .map_err(|_| invalid("invalid Content-Length"))?,
        )
    } else {
        Framing::UntilClose
    };
    Ok(Response {
        status,
        headers,
        body: Body { reader, framing },
    })
}

/// A response body as a byte stream: chunked transfer coding is removed, a
/// `Content-Length` body ends after that many bytes, and any other body ends
/// when the server closes the connection.
struct Body<R = BufReader<Conn>> {
    reader: R,
    framing: Framing,
}

#[derive(Debug, Clone, Copy)]
enum Framing {
    Length(u64),
    Chunked { remaining: u64, done: bool },
    UntilClose,
}

impl<R: BufRead> Body<R> {
    /// The size line of the next chunk; `0` marks the last chunk.
    fn chunk_size(&mut self) -> io::Result<u64> {
        let mut line = Vec::new();
        read_line_capped(&mut self.reader, &mut line, MAX_HEAD_BYTES)?;
        if !line.ends_with(b"\n") {
            return Err(invalid("truncated chunk size"));
        }
        let text = String::from_utf8_lossy(&line);
        let digits = text.split(';').next().unwrap_or_default().trim();
        u64::from_str_radix(digits, 16).map_err(|_| invalid("invalid chunk size"))
    }

    /// Skip the trailer section after the last chunk. A server that closes
    /// the connection right after the `0` line is tolerated.
    fn skip_trailers(&mut self) -> io::Result<()> {
        loop {
            let mut line = Vec::new();
            read_line_capped(&mut self.reader, &mut line, MAX_HEAD_BYTES)?;
            if line.is_empty() || line == b"\r\n" || line == b"\n" {
                return Ok(());
            }
        }
    }
}

impl<R: BufRead> Read for Body<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::UntilClose => self.reader.read(buf),
            Framing::Length(0) | Framing::Chunked { done: true, .. } => Ok(0),
            Framing::Length(remaining) => {
                let want = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
                let n = self.reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "body shorter than its Content-Length",
                    ));
                }
                self.framing = Framing::Length(remaining - n as u64);
                Ok(n)
            }
            Framing::Chunked { remaining, .. } => {
                let remaining = if remaining == 0 {
                    let size = self.chunk_size()?;
                    if size == 0 {
                        self.skip_trailers()?;
                        self.framing = Framing::Chunked {
                            remaining: 0,
                            done: true,
                        };
                        return Ok(0);
                    }
                    size
                } else {
                    remaining
                };
                let want = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
                let n = self.reader.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated chunk",
                    ));
                }
                let left = remaining - n as u64;
                if left == 0 {
                    let mut crlf = [0u8; 2];
                    self.reader.read_exact(&mut crlf)?;
                    if &crlf != b"\r\n" {
                        return Err(invalid("missing chunk terminator"));
                    }
                }
                self.framing = Framing::Chunked {
                    remaining: left,
                    done: false,
                };
                Ok(n)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    /// What the test server saw: the raw request head and body.
    struct Seen {
        head: String,
        body: Vec<u8>,
        connections: usize,
    }

    /// Accept up to `replies.len()` connections on loopback, each bounded by
    /// a 10 s accept window and 10 s socket timeouts, read one request and
    /// write the matching canned reply (`None`: hold the connection open
    /// without answering until the client gives up). Never hangs.
    fn serve(replies: Vec<Option<Vec<u8>>>) -> (String, JoinHandle<Seen>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let mut seen = Seen {
                head: String::new(),
                body: Vec::new(),
                connections: 0,
            };
            for reply in replies {
                let accept_by = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            if Instant::now() >= accept_by {
                                return seen;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return seen,
                    }
                };
                seen.connections += 1;
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                seen.head = head;
                seen.body = body;
                match reply {
                    Some(bytes) => {
                        let _ = stream.write_all(&bytes);
                        // Send a graceful EOF before closing both cloned socket handles on Windows.
                        let _ = stream.shutdown(std::net::Shutdown::Write);
                    }
                    // Hold the socket until the client closes it (or 10 s).
                    None => {
                        let _ = reader.read_to_end(&mut Vec::new());
                    }
                }
            }
            seen
        });
        (addr, handle)
    }

    #[test]
    fn post_sends_framed_request_and_reads_length_body() {
        let (addr, server) = serve(vec![Some(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: abc\r\nContent-Length: 11\r\n\r\n{\"ok\":true}".to_vec(),
        )]);
        let url = format!("http://{addr}/mcp");
        let resp = call(
            "POST",
            &url,
            &[("accept", "application/json"), ("x-susi-node", "n1")],
            Some(b"{\"id\":1}"),
            5,
        )
        .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.header("MCP-SESSION-ID"), Some("abc"));
        assert_eq!(resp.header("missing"), None);
        assert_eq!(resp.into_bytes(1024).unwrap(), b"{\"ok\":true}");
        let seen = server.join().unwrap();
        assert!(
            seen.head.starts_with("POST /mcp HTTP/1.1\r\n"),
            "{}",
            seen.head
        );
        assert!(seen.head.contains(&format!("Host: {addr}\r\n")));
        assert!(seen.head.contains("Connection: close\r\n"));
        assert!(seen.head.contains("Content-Length: 8\r\n"));
        assert!(seen.head.contains("accept: application/json\r\n"));
        assert!(seen.head.contains("x-susi-node: n1\r\n"));
        assert_eq!(seen.body, b"{\"id\":1}");
    }

    #[test]
    fn chunked_body_is_decoded_with_extensions_and_trailers() {
        let (addr, server) = serve(vec![Some(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6;ext=1\r\ndata: \r\n5\r\nhello\r\n0\r\nx-trailer: 1\r\n\r\n".to_vec(),
        )]);
        let resp = call("POST", &format!("http://{addr}/mcp"), &[], Some(b""), 5).unwrap();
        assert_eq!(resp.into_bytes(1024).unwrap(), b"data: hello");
        let seen = server.join().unwrap();
        assert!(seen.head.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn body_without_framing_ends_at_close_and_interim_heads_are_skipped() {
        let (addr, server) = serve(vec![Some(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\nrest of body".to_vec(),
        )]);
        let resp = call("POST", &format!("http://{addr}/"), &[], Some(b"x"), 5).unwrap();
        assert_eq!(resp.status, 202);
        assert_eq!(resp.into_bytes(1024).unwrap(), b"rest of body");
        server.join().unwrap();
    }

    #[test]
    fn error_statuses_are_returned_and_redirects_are_not_followed() {
        let (addr, server) = serve(vec![
            Some(
                b"HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\n\r\n".to_vec(),
            ),
            Some(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope".to_vec()),
        ]);
        let first = call("POST", &format!("http://{addr}/mcp"), &[], Some(b"{}"), 5).unwrap();
        assert_eq!(first.status, 302);
        assert_eq!(first.header("location"), Some("/elsewhere"));
        assert!(first.into_bytes(16).unwrap().is_empty());
        let second = call("POST", &format!("http://{addr}/mcp"), &[], Some(b"{}"), 5).unwrap();
        assert_eq!(second.status, 404);
        assert_eq!(second.into_bytes(16).unwrap(), b"nope");
        // Exactly the two requests made: the 302 was not chased.
        assert_eq!(server.join().unwrap().connections, 2);
    }

    #[test]
    fn delete_without_body_sends_no_content_length_and_204_has_no_body() {
        let (addr, server) = serve(vec![Some(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec())]);
        let resp = call(
            "DELETE",
            &format!("http://{addr}/mcp"),
            &[("mcp-session-id", "abc")],
            None,
            5,
        )
        .unwrap();
        assert_eq!(resp.status, 204);
        assert!(resp.into_bytes(16).unwrap().is_empty());
        let seen = server.join().unwrap();
        assert!(seen.head.starts_with("DELETE /mcp HTTP/1.1\r\n"));
        assert!(!seen.head.to_ascii_lowercase().contains("content-length"));
        assert!(seen.head.contains("mcp-session-id: abc\r\n"));
    }

    #[test]
    fn body_cap_reads_one_extra_byte_and_refuses_overflow() {
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n12345".to_vec();
        let (addr, server) = serve(vec![Some(reply.clone()), Some(reply)]);
        let url = format!("http://{addr}/");
        let exact = call("GET", &url, &[], None, 5).unwrap();
        assert_eq!(exact.into_bytes(5).unwrap(), b"12345");
        let over = call("GET", &url, &[], None, 5).unwrap();
        let err = over.into_bytes(4).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds configured limit"));
        server.join().unwrap();
    }

    #[test]
    fn truncated_and_malformed_bodies_fail() {
        let (addr, server) = serve(vec![
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort".to_vec()),
            Some(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n".to_vec()),
            Some(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcXY0\r\n\r\n"
                    .to_vec(),
            ),
            Some(b"garbage\r\n\r\n".to_vec()),
        ]);
        let url = format!("http://{addr}/");
        let short = call("GET", &url, &[], None, 5).unwrap();
        assert_eq!(
            short.into_bytes(64).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let bad_size = call("GET", &url, &[], None, 5).unwrap();
        assert!(bad_size
            .into_bytes(64)
            .unwrap_err()
            .to_string()
            .contains("invalid chunk size"));
        let bad_end = call("GET", &url, &[], None, 5).unwrap();
        assert!(bad_end
            .into_bytes(64)
            .unwrap_err()
            .to_string()
            .contains("missing chunk terminator"));
        let err = call("GET", &url, &[], None, 5).err().unwrap();
        assert!(err.contains("not HTTP/1.x"), "{err}");
        server.join().unwrap();
    }

    #[test]
    fn a_silent_server_costs_the_budget_not_more() {
        let (addr, server) = serve(vec![None]);
        let started = Instant::now();
        let err = call_within(
            "POST",
            &format!("http://{addr}/mcp"),
            &[],
            Some(b"{}"),
            Duration::from_millis(120),
        )
        .err()
        .unwrap();
        let took = started.elapsed();
        assert!(err.contains("time budget"), "{err}");
        assert!(took >= Duration::from_millis(80), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        server.join().unwrap();
    }

    #[test]
    fn the_budget_also_bounds_a_body_that_never_finishes() {
        let (tx, rx) = mpsc::channel::<()>();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
                .unwrap();
            // Keep the socket open until the client is done (bounded).
            let _ = rx.recv_timeout(Duration::from_secs(10));
        });
        let started = Instant::now();
        let resp = call_within(
            "GET",
            &format!("http://{addr}/"),
            &[],
            None,
            Duration::from_millis(150),
        )
        .unwrap();
        let err = resp.into_bytes(1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn https_and_other_schemes_are_refused_before_connecting() {
        let err = call("POST", "https://127.0.0.1:1/mcp", &[], None, 1)
            .err()
            .unwrap();
        assert!(err.contains("https:// is not supported"), "{err}");
        let err = call("GET", "ftp://host/x", &[], None, 1).err().unwrap();
        assert!(err.contains("unsupported URL scheme"), "{err}");
        let err = call("GET", "127.0.0.1:80/x", &[], None, 1).err().unwrap();
        assert!(err.contains("not an absolute http:// URL"), "{err}");
    }

    #[test]
    fn request_injection_is_refused() {
        let err = call(
            "POST",
            "http://127.0.0.1:1/",
            &[("x-a", "1\r\nevil: 2")],
            None,
            1,
        )
        .err()
        .unwrap();
        assert!(err.contains("invalid request header"), "{err}");
        let err = call("GET /x HTTP/1.1\r\n", "http://127.0.0.1:1/", &[], None, 1)
            .err()
            .unwrap();
        assert!(err.contains("unsupported HTTP method"), "{err}");
    }

    #[test]
    fn urls_parse_hosts_ports_and_paths() {
        let t = parse_url("http://127.0.0.1:9093/mcp").unwrap();
        assert_eq!(
            (
                t.host.as_str(),
                t.port,
                t.authority.as_str(),
                t.path.as_str()
            ),
            ("127.0.0.1", 9093, "127.0.0.1:9093", "/mcp")
        );
        let t = parse_url("http://[::1]:9093/mcp?x=1#frag").unwrap();
        assert_eq!(
            (
                t.host.as_str(),
                t.port,
                t.authority.as_str(),
                t.path.as_str()
            ),
            ("::1", 9093, "[::1]:9093", "/mcp?x=1")
        );
        let t = parse_url("HTTP://peer.lan").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str()),
            ("peer.lan", 80, "/")
        );
        let t = parse_url("http://peer.lan?q").unwrap();
        assert_eq!(t.path, "/?q");
        assert!(parse_url("http://host:notaport/").is_err());
        assert!(parse_url("http://[::1/").is_err());
        assert!(parse_url("http:///mcp").is_err());
        assert!(parse_url("http://user@host/").is_err());
    }

    #[test]
    fn a_refused_connection_is_an_error_naming_the_target() {
        // Bind then drop: the port is closed (nothing listens on it).
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let err = call("GET", &format!("http://{addr}/"), &[], None, 2)
            .err()
            .unwrap();
        assert!(err.starts_with(&format!("connect {addr}")), "{err}");
    }
}
