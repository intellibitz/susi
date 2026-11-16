//! Minimal HTTP/1.1 client over `std::net`: one request per connection
//! (`Connection: close`, no keep-alive), with the body either read whole
//! ([`request`]) or decoded incrementally as it arrives
//! ([`request_streaming`]), so a Server-Sent Events reply can be consumed
//! while the server is still writing it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use susi_paths::http_body::{read_line_capped, MAX_HEAD_BYTES};

/// Streaming body with the loopback client's historical error diagnostics.
pub type Body<R = BufReader<Conn>> = susi_paths::http_body::Body<R>;

/// A complete response: status, lower-cased headers and decoded body.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Header names lower-cased, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Body with any chunked transfer coding removed.
    pub body: Vec<u8>,
}

impl Response {
    /// First header value named `name` (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The body as text (lossy).
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A response whose head has been read and whose body is still on the
/// wire: [`Body`] decodes it on each `read`.
#[derive(Debug)]
pub struct StreamingResponse<R = BufReader<Conn>> {
    /// HTTP status code.
    pub status: u16,
    /// Header names lower-cased, in arrival order.
    pub headers: Vec<(String, String)>,
    /// The body, transfer coding removed as it is read.
    pub body: Body<R>,
}

impl<R> StreamingResponse<R> {
    /// First header value named `name` (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    let name = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.as_str())
}

/// One outbound request.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Absolute path.
    pub path: &'a str,
    /// Extra headers.
    pub headers: &'a [(&'a str, &'a str)],
    /// Optional body.
    pub body: Option<&'a [u8]>,
}

/// Send `req` with `Connection: close` and read the whole response.
pub fn request(
    host: &str,
    port: u16,
    req: Request<'_>,
    timeout: Duration,
) -> Result<Response, String> {
    let StreamingResponse {
        status,
        headers,
        mut body,
    } = request_streaming(host, port, req, timeout)?;
    let mut decoded = Vec::new();
    body.read_to_end(&mut decoded)
        .map_err(|e| format!("read from {host}:{port}: {e}"))?;
    Ok(Response {
        status,
        headers,
        body: decoded,
    })
}

/// Send `req` with `Connection: close` and return once the response head
/// has arrived. `timeout` bounds the connect and every individual read and
/// write, so a stream stays open as long as the server keeps writing.
pub fn request_streaming(
    host: &str,
    port: u16,
    req: Request<'_>,
    timeout: Duration,
) -> Result<StreamingResponse, String> {
    request_streaming_by(host, port, req, timeout, None)
}

/// [`request_streaming`] that also fails every read once `deadline` has
/// passed, so a server that keeps a stream alive (keep-alive comments,
/// progress) cannot hold the caller past its total budget.
pub fn request_streaming_by(
    host: &str,
    port: u16,
    req: Request<'_>,
    timeout: Duration,
    deadline: Option<Instant>,
) -> Result<StreamingResponse, String> {
    let Request {
        method,
        path,
        headers,
        body,
    } = req;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}:{port}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}:{port}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let body = body.unwrap_or_default();
    // An IPv6 literal needs brackets in the Host header.
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| format!("send to {host}:{port}: {e}"))?;
    let conn = Conn {
        stream,
        idle: timeout,
        deadline,
    };
    read_response(BufReader::new(conn), method).map_err(|e| format!("read from {host}:{port}: {e}"))
}

/// The connection a streaming response reads from: each read waits at most
/// the idle timeout, and never past the deadline when one is set.
#[derive(Debug)]
pub struct Conn {
    stream: TcpStream,
    idle: Duration,
    deadline: Option<Instant>,
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(deadline) = self.deadline else {
            return self.stream.read(buf);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(over_budget());
        }
        self.stream.set_read_timeout(Some(left.min(self.idle)))?;
        // A socket timeout that ends at the deadline is the budget running
        // out, whichever error kind the OS reports for it.
        self.stream.read(buf).map_err(|e| {
            let timed_out = matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            );
            if timed_out && Instant::now() >= deadline {
                over_budget()
            } else {
                e
            }
        })
    }
}

fn over_budget() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "response did not complete within its time budget",
    )
}

/// Read a response head from `reader` and frame the body that follows.
/// `method` matters only for `HEAD`, whose response never has a body.
pub fn read_response<R: BufRead>(mut reader: R, method: &str) -> io::Result<StreamingResponse<R>> {
    let mut lines = Vec::new();
    let mut used = 0;
    loop {
        let mut line = Vec::new();
        read_line_capped(&mut reader, &mut line, MAX_HEAD_BYTES - used)?;
        used += line.len() as u64;
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
    let status: u16 = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1).map(str::to_string))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| invalid("response has no status line"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let length = header(&headers, "content-length").and_then(|v| v.parse::<u64>().ok());
    let no_body = method.eq_ignore_ascii_case("HEAD")
        || (100..200).contains(&status)
        || status == 204
        || status == 304;
    let body = if no_body {
        Body::with_length(reader, 0)
    } else if chunked {
        Body::chunked(reader)
    } else if let Some(length) = length {
        Body::with_length(reader, length)
    } else {
        Body::until_close(reader)
    };
    Ok(StreamingResponse {
        status,
        headers,
        body,
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// A whole response held in memory, decoded the way [`request`] does.
#[cfg(test)]
fn parse(raw: &[u8]) -> Result<Response, String> {
    let StreamingResponse {
        status,
        headers,
        mut body,
    } = read_response(raw, "GET").map_err(|e| e.to_string())?;
    let mut decoded = Vec::new();
    body.read_to_end(&mut decoded).map_err(|e| e.to_string())?;
    Ok(Response {
        status,
        headers,
        body: decoded,
    })
}

/// A chunked body held in memory, decoded the way [`Body`] does.
#[cfg(test)]
fn dechunk(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    Body::chunked(data)
        .read_to_end(&mut decoded)
        .map_err(|e| e.to_string())?;
    Ok(decoded)
}

#[cfg(test)]
#[path = "tests/http.rs"]
mod tests;
