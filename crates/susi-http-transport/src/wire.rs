//! HTTP/1.1 wire format for the outbound client: request targets, the
//! request head, the response head and the three response body framings
//! (`Content-Length`, `Transfer-Encoding: chunked`, read-until-close).
//!
//! Std only. Every read goes through the caller's stream, whose socket
//! carries the request deadline (`connect::TimedSocket`), so nothing here
//! can block past the caller's budget.

use std::io::{self, BufRead, Read};

/// Response heads larger than this are refused (the bound ureq applied).
pub(crate) const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Longest chunk-size line accepted inside a chunked body.
const MAX_CHUNK_LINE: usize = 4 * 1024;

/// URL scheme of a request target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scheme {
    Http,
    Https,
}

impl Scheme {
    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// A parsed `http://` or `https://` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub scheme: Scheme,
    /// Host as written in the URL, without IPv6 brackets.
    pub host: String,
    pub port: u16,
    /// Whether the URL named the port explicitly.
    pub explicit_port: bool,
    /// Path and query, always starting with `/`; the fragment is dropped.
    pub path_and_query: String,
}

impl Target {
    pub fn parse(url: &str) -> Result<Self, String> {
        if url.bytes().any(|b| b <= b' ' || b >= 0x7f) {
            return Err(format!(
                "invalid URL {url:?}: whitespace, control or non-ASCII byte"
            ));
        }
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| format!("invalid URL {url:?}: no scheme"))?;
        let scheme = if scheme.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else if scheme.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else {
            return Err(format!("unsupported URL scheme {scheme:?} in {url:?}"));
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let split = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(split);
        // Userinfo is never sent: credentials travel in headers.
        let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = split_host_port(hostport)
            .ok_or_else(|| format!("invalid URL {url:?}: bad host or port"))?;
        if host.is_empty() {
            return Err(format!("invalid URL {url:?}: empty host"));
        }
        let path_and_query = match tail.chars().next() {
            Some('/') => tail.to_string(),
            Some(_) => format!("/{tail}"),
            None => "/".to_string(),
        };
        Ok(Self {
            scheme,
            host: host.to_string(),
            port: port.unwrap_or(scheme.default_port()),
            explicit_port: port.is_some(),
            path_and_query,
        })
    }

    /// Host as it appears in a URL authority (IPv6 literals bracketed).
    pub fn host_for_authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// `Host` header value: the port only when it is not the default.
    pub fn host_header(&self) -> String {
        if self.port == self.scheme.default_port() {
            self.host_for_authority()
        } else {
            format!("{}:{}", self.host_for_authority(), self.port)
        }
    }

    /// `host:port` as a CONNECT request target.
    pub fn connect_authority(&self) -> String {
        format!("{}:{}", self.host_for_authority(), self.port)
    }

    pub fn is_https(&self) -> bool {
        self.scheme == Scheme::Https
    }

    fn origin(&self) -> String {
        if self.explicit_port {
            format!(
                "{}://{}:{}",
                self.scheme.as_str(),
                self.host_for_authority(),
                self.port
            )
        } else {
            format!("{}://{}", self.scheme.as_str(), self.host_for_authority())
        }
    }

    /// Resolve a redirect `Location` against this target (RFC 3986 §5.2).
    pub fn join(&self, location: &str) -> Result<Target, String> {
        let location = location.trim();
        let has_scheme = location.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
                && scheme.as_bytes()[0].is_ascii_alphabetic()
        });
        let absolute = if has_scheme {
            location.to_string()
        } else if let Some(rest) = location.strip_prefix("//") {
            format!("{}://{rest}", self.scheme.as_str())
        } else if location.starts_with('/') {
            format!("{}{}", self.origin(), remove_dot_segments(location))
        } else if location.starts_with('?') {
            let path = self.path_and_query.split('?').next().unwrap_or("/");
            format!("{}{path}{location}", self.origin())
        } else {
            let path = self.path_and_query.split('?').next().unwrap_or("/");
            let dir = &path[..path.rfind('/').map_or(0, |at| at + 1)];
            format!(
                "{}{}",
                self.origin(),
                remove_dot_segments(&format!("{dir}{location}"))
            )
        };
        Target::parse(&absolute)
    }
}

/// Split `host[:port]` / `[v6][:port]`. `None` when malformed.
fn split_host_port(hostport: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = hostport.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        return match after.strip_prefix(':') {
            Some("") => Some((host, None)),
            Some(port) => Some((host, Some(port.parse().ok()?))),
            None if after.is_empty() => Some((host, None)),
            None => None,
        };
    }
    match hostport.rsplit_once(':') {
        Some((host, "")) => Some((host, None)),
        Some((host, port)) if !host.contains(':') => Some((host, Some(port.parse().ok()?))),
        Some(_) => None,
        None => Some((hostport, None)),
    }
}

/// RFC 3986 §5.2.4 on a path (with any query kept intact).
fn remove_dot_segments(path_and_query: &str) -> String {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        match *segment {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    let mut joined = out.join("/");
    if !joined.starts_with('/') {
        joined.insert(0, '/');
    }
    match query {
        Some(q) => format!("{joined}?{q}"),
        None => joined,
    }
}

/// RFC 9110 token characters (header names, methods).
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// A field value may not carry CR, LF, NUL or other controls (header
/// injection); tab and obs-text are allowed, as `http::HeaderValue` allows.
fn is_field_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| (b >= 0x20 && b != 0x7f) || b == b'\t')
}

/// Serialize a request head. `headers` are the caller's; `Host`,
/// `User-Agent` and `Accept` are added when absent, `Content-Length` is
/// always computed here and `Connection: close` always sent (the client
/// does not pool connections).
pub(crate) fn request_head(
    method: &str,
    target: &Target,
    headers: &[(String, String)],
    body_len: Option<usize>,
) -> Result<Vec<u8>, String> {
    let mut head = format!("{method} {} HTTP/1.1\r\n", target.path_and_query);
    let has = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !has("host") {
        head.push_str(&format!("Host: {}\r\n", target.host_header()));
    }
    if !has("user-agent") {
        head.push_str(&format!("User-Agent: {}\r\n", crate::client::USER_AGENT));
    }
    if !has("accept") {
        head.push_str("Accept: */*\r\n");
    }
    for (name, value) in headers {
        if !is_token(name) {
            return Err(format!("invalid header name {name:?}"));
        }
        if !is_field_value(value) {
            return Err(format!("invalid value for header {name}"));
        }
        if ["content-length", "transfer-encoding", "connection"]
            .iter()
            .any(|framing| name.eq_ignore_ascii_case(framing))
        {
            continue;
        }
        head.push_str(&format!("{name}: {}\r\n", value.trim()));
    }
    if let Some(len) = body_len {
        head.push_str(&format!("Content-Length: {len}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    Ok(head.into_bytes())
}

/// A parsed response head.
#[derive(Debug)]
pub(crate) struct Head {
    pub status: u16,
    /// Lowercase names; values that are not visible ASCII are left out, as
    /// `http::HeaderValue::to_str` left them out before.
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Read one CRLF (or bare LF) terminated line, counting it against `budget`.
fn read_line(reader: &mut impl BufRead, budget: &mut usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the response head ended",
            ));
        }
        let (take, done) = match available.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (available.len(), false),
        };
        if take > *budget {
            return Err(invalid("response head exceeds 64 KiB"));
        }
        *budget -= take;
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if done {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
    }
}

/// Read a response head, skipping interim 1xx responses.
pub(crate) fn read_head(reader: &mut impl BufRead) -> io::Result<Head> {
    let mut budget = MAX_HEAD_BYTES;
    loop {
        let head = read_one_head(reader, &mut budget)?;
        match head.status {
            101 => return Err(invalid("unexpected 101 Switching Protocols")),
            100..=199 => continue,
            _ => return Ok(head),
        }
    }
}

fn read_one_head(reader: &mut impl BufRead, budget: &mut usize) -> io::Result<Head> {
    let line = read_line(reader, budget)?;
    let line = String::from_utf8(line).map_err(|_| invalid("status line is not UTF-8"))?;
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(invalid(format!("unsupported response version {version:?}")));
    }
    let status = parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (100..=999).contains(code))
        .ok_or_else(|| invalid(format!("malformed status line {line:?}")))?;
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader, budget)?;
        if line.is_empty() {
            return Ok(Head { status, headers });
        }
        if matches!(line.first(), Some(b' ' | b'\t')) {
            return Err(invalid("obsolete header line folding"));
        }
        let colon = line
            .iter()
            .position(|&b| b == b':')
            .ok_or_else(|| invalid("header line without a colon"))?;
        let name = std::str::from_utf8(&line[..colon])
            .ok()
            .filter(|name| is_token(name))
            .ok_or_else(|| invalid("invalid header name in response"))?
            .to_ascii_lowercase();
        let value = &line[colon + 1..];
        let visible = value
            .iter()
            .all(|&b| (0x20..0x7f).contains(&b) || b == b'\t');
        if visible {
            // Visible ASCII is UTF-8.
            let value = String::from_utf8_lossy(value).trim().to_string();
            headers.push((name, value));
        }
    }
}

/// How the response body is delimited (RFC 9112 §6.3).
pub(crate) enum Framing {
    Empty,
    Length(u64),
    Chunked,
    UntilClose,
}

pub(crate) fn framing(method: &str, head: &Head) -> io::Result<Framing> {
    if method == "HEAD" || matches!(head.status, 100..=199 | 204 | 304) {
        return Ok(Framing::Empty);
    }
    if let Some(te) = head.header("transfer-encoding") {
        let last = te.rsplit(',').next().unwrap_or_default().trim();
        return Ok(if last.eq_ignore_ascii_case("chunked") {
            Framing::Chunked
        } else {
            Framing::UntilClose
        });
    }
    let mut length = None;
    for (name, value) in &head.headers {
        if name != "content-length" {
            continue;
        }
        for item in value.split(',') {
            let parsed: u64 = item
                .trim()
                .parse()
                .map_err(|_| invalid(format!("invalid Content-Length {value:?}")))?;
            if length.is_some_and(|seen| seen != parsed) {
                return Err(invalid("conflicting Content-Length values"));
            }
            length = Some(parsed);
        }
    }
    Ok(match length {
        Some(0) => Framing::Empty,
        Some(len) => Framing::Length(len),
        None => Framing::UntilClose,
    })
}

/// Body reader over the connection's buffered reader.
pub(crate) struct BodyReader<R> {
    inner: R,
    state: BodyState,
    /// A chunk's data has been read, so its closing CRLF comes next.
    after_chunk: bool,
}

enum BodyState {
    Done,
    Length(u64),
    /// Bytes left in the current chunk; 0 means a size line comes next.
    Chunk(u64),
    UntilClose,
}

impl<R: BufRead> BodyReader<R> {
    pub fn new(inner: R, framing: Framing) -> Self {
        let state = match framing {
            Framing::Empty => BodyState::Done,
            Framing::Length(len) => BodyState::Length(len),
            Framing::Chunked => BodyState::Chunk(0),
            Framing::UntilClose => BodyState::UntilClose,
        };
        Self {
            inner,
            state,
            after_chunk: false,
        }
    }

    fn next_chunk(&mut self, first: bool) -> io::Result<u64> {
        let mut budget = MAX_CHUNK_LINE;
        if !first {
            // The CRLF closing the previous chunk's data.
            let line = read_line(&mut self.inner, &mut budget).map_err(truncated_chunk)?;
            if !line.is_empty() {
                return Err(invalid("chunk data not followed by CRLF"));
            }
            budget = MAX_CHUNK_LINE;
        }
        let line = read_line(&mut self.inner, &mut budget).map_err(truncated_chunk)?;
        let line = String::from_utf8(line).map_err(|_| invalid("chunk size is not UTF-8"))?;
        let size = line.split(';').next().unwrap_or_default().trim();
        u64::from_str_radix(size, 16).map_err(|_| invalid(format!("invalid chunk size {size:?}")))
    }

    fn skip_trailers(&mut self) -> io::Result<()> {
        let mut budget = MAX_HEAD_BYTES;
        loop {
            if read_line(&mut self.inner, &mut budget)
                .map_err(truncated_chunk)?
                .is_empty()
            {
                return Ok(());
            }
        }
    }
}

fn truncated_chunk(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "response body ended before the final chunk",
        )
    } else {
        error
    }
}

impl<R: BufRead> Read for BodyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                BodyState::Done => return Ok(0),
                BodyState::UntilClose => return self.inner.read(buf),
                BodyState::Length(left) => {
                    let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let got = self.inner.read(&mut buf[..want])?;
                    if got == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("response body ended {left} bytes before its Content-Length"),
                        ));
                    }
                    let left = left - got as u64;
                    self.state = if left == 0 {
                        BodyState::Done
                    } else {
                        BodyState::Length(left)
                    };
                    return Ok(got);
                }
                BodyState::Chunk(0) => {
                    let size = self.next_chunk(!self.after_chunk)?;
                    self.after_chunk = true;
                    if size == 0 {
                        self.skip_trailers()?;
                        self.state = BodyState::Done;
                        return Ok(0);
                    }
                    self.state = BodyState::Chunk(size);
                }
                BodyState::Chunk(left) => {
                    let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let got = self.inner.read(&mut buf[..want])?;
                    if got == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "response body ended before the final chunk",
                        ));
                    }
                    let left = left - got as u64;
                    // A finished chunk reads its CRLF with the next size line.
                    self.state = if left == 0 {
                        BodyState::Chunk(0)
                    } else {
                        BodyState::Chunk(left)
                    };
                    return Ok(got);
                }
            }
        }
    }
}
