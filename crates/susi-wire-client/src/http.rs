//! Minimal HTTP/1.1 client over `std::net`: one request per connection
//! (`Connection: close`, no keep-alive), with the body either read whole
//! ([`request`]) or decoded incrementally as it arrives
//! ([`request_streaming`]), so a Server-Sent Events reply can be consumed
//! while the server is still writing it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Cap on a response head (status line plus headers) and on one chunk-size
/// or trailer line, so a peer that never sends a newline cannot grow memory
/// without limit.
const MAX_HEAD_BYTES: u64 = 64 * 1024;

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

/// Read one line ending in `\n` into `line`, refusing more than `limit`
/// bytes without a newline.
fn read_line_capped<R: BufRead>(reader: &mut R, line: &mut Vec<u8>, limit: u64) -> io::Result<()> {
    reader.take(limit).read_until(b'\n', line)?;
    if !line.ends_with(b"\n") && line.len() as u64 >= limit {
        return Err(invalid("response line too long"));
    }
    Ok(())
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
    let framing = if no_body {
        Framing::Length(0)
    } else if chunked {
        Framing::Chunked {
            remaining: 0,
            done: false,
        }
    } else if let Some(length) = length {
        Framing::Length(length)
    } else {
        Framing::UntilClose
    };
    Ok(StreamingResponse {
        status,
        headers,
        body: Body { reader, framing },
    })
}

/// A response body as a byte stream: chunked transfer coding is removed,
/// a `Content-Length` body ends after that many bytes, and any other body
/// ends when the server closes the connection.
#[derive(Debug)]
pub struct Body<R = BufReader<Conn>> {
    reader: R,
    framing: Framing,
}

#[derive(Debug)]
enum Framing {
    Length(u64),
    Chunked { remaining: u64, done: bool },
    UntilClose,
}

impl<R: BufRead> Body<R> {
    /// A chunked body read from `reader` (the bytes after the head).
    pub fn chunked(reader: R) -> Self {
        Self {
            reader,
            framing: Framing::Chunked {
                remaining: 0,
                done: false,
            },
        }
    }

    /// The size line of the next chunk; `0` marks the last chunk.
    fn chunk_size(&mut self) -> io::Result<u64> {
        let mut line = Vec::new();
        read_line_capped(&mut self.reader, &mut line, MAX_HEAD_BYTES)?;
        if !line.ends_with(b"\n") {
            return Err(invalid("truncated chunk size"));
        }
        let text = String::from_utf8_lossy(&line);
        let digits = text.split(';').next().unwrap_or("").trim();
        u64::from_str_radix(digits, 16)
            .map_err(|_| invalid(&format!("invalid chunk size {:?}", text.trim_end())))
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
                    return Err(invalid("truncated chunk"));
                }
                let left = remaining - n as u64;
                if left == 0 {
                    let mut crlf = [0u8; 2];
                    match self.reader.read_exact(&mut crlf) {
                        Ok(()) if &crlf == b"\r\n" => {}
                        Ok(()) => return Err(invalid("missing chunk terminator")),
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                            return Err(invalid("missing chunk terminator"));
                        }
                        // A timeout keeps its own kind and message.
                        Err(e) => return Err(e),
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
mod tests {
    use super::*;

    #[test]
    fn parses_length_and_chunked_bodies() {
        let plain = parse(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello").unwrap();
        assert_eq!((plain.status, plain.text().as_str()), (200, "hello"));
        assert_eq!(plain.header("CONTENT-TYPE"), Some("text/plain"));
        let chunked =
            parse(b"HTTP/1.1 401 X\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!((chunked.status, chunked.text().as_str()), (401, "abcde"));
        assert!(parse(b"garbage").is_err());
    }

    #[test]
    fn dechunk_rejects_truncated_size_invalid_hex_truncated_payload_and_missing_crlf() {
        // 1. Truncated chunk size line (no CRLF)
        let err_no_crlf = dechunk(b"4").unwrap_err();
        assert!(
            err_no_crlf.contains("truncated chunk size"),
            "{err_no_crlf}"
        );

        // 2. Invalid hex chunk size
        let err_hex = dechunk(b"zz\r\n").unwrap_err();
        assert!(err_hex.contains("invalid chunk size"), "{err_hex}");

        // 3. Payload shorter than declared hex size
        let err_short = dechunk(b"5\r\nabc").unwrap_err();
        assert!(err_short.contains("truncated chunk"), "{err_short}");

        // 4. Payload not followed by CRLF
        let err_term = dechunk(b"4\r\ntestXX0\r\n\r\n").unwrap_err();
        assert!(err_term.contains("missing chunk terminator"), "{err_term}");

        // 5. Payload ends at exact size without trailing CRLF
        let err_eof = dechunk(b"4\r\ntest").unwrap_err();
        assert!(err_eof.contains("missing chunk terminator"), "{err_eof}");

        // 6. Valid chunk with extension parameters
        let decoded = dechunk(b"4;ext=val\r\ntest\r\n0\r\n\r\n").unwrap();
        assert_eq!(decoded, b"test");
    }

    #[test]
    fn parse_rejects_missing_header_terminator_and_invalid_status_line_and_normalizes_headers() {
        // Missing header terminator
        let err_no_term = parse(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n").unwrap_err();
        assert!(
            err_no_term.contains("no header terminator"),
            "{err_no_term}"
        );

        // Non-numeric status code
        let err_bad_status = parse(b"HTTP/1.1 TWO_HUNDRED OK\r\n\r\n").unwrap_err();
        assert!(
            err_bad_status.contains("no status line"),
            "{err_bad_status}"
        );

        // Missing status code
        let err_missing_code = parse(b"HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(
            err_missing_code.contains("no status line"),
            "{err_missing_code}"
        );

        // Multi-token Transfer-Encoding: gzip, Chunked + case-insensitive header lookup
        let resp = parse(
            b"HTTP/1.1 202 Accepted\r\nX-Susi-Node: alpha\r\nTransfer-Encoding: gzip, Chunked\r\n\r\n4\r\npong\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(resp.status, 202);
        assert_eq!(resp.header("x-susi-node"), Some("alpha"));
        assert_eq!(resp.header("X-SUSI-NODE"), Some("alpha"));
        assert_eq!(resp.header("missing"), None);
        assert_eq!(resp.text(), "pong");
    }

    #[test]
    fn request_sends_formatted_http11_post_and_decodes_chunked_response_over_loopback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 512];
            let mut header_end = None;
            let mut content_len = 0usize;
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let head = String::from_utf8_lossy(&buf[..pos]);
                        for line in head.lines() {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("content-length") {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_len {
                        break;
                    }
                }
            }
            let raw_reply = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\n{\"ok\":1\r\n1\r\n}\r\n0\r\n\r\n";
            stream.write_all(raw_reply).unwrap();
            stream.flush().unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            String::from_utf8_lossy(&buf).into_owned()
        });

        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = request(
            "127.0.0.1",
            port,
            Request {
                method: "POST",
                path: "/mcp",
                headers: &[
                    ("Authorization", "Bearer tok-123"),
                    ("Content-Type", "application/json"),
                ],
                body: Some(body),
            },
            Duration::from_secs(2),
        )
        .unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.header("content-type"), Some("application/json"));
        assert_eq!(resp.text(), "{\"ok\":1}");

        let received = handle.join().unwrap();
        assert!(received.starts_with("POST /mcp HTTP/1.1\r\n"), "{received}");
        assert!(
            received.contains(&format!("Host: 127.0.0.1:{port}\r\n")),
            "{received}"
        );
        assert!(received.contains("Connection: close\r\n"), "{received}");
        assert!(
            received.contains(&format!("Content-Length: {}\r\n", body.len())),
            "{received}"
        );
        assert!(
            received.contains("Authorization: Bearer tok-123\r\n"),
            "{received}"
        );
        assert!(
            received.ends_with("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}"),
            "{received}"
        );
    }

    #[test]
    fn framing_honours_content_length_head_and_bodiless_statuses() {
        let raw: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcEXTRA";
        assert_eq!(parse(raw).unwrap().text(), "abc");
        let short = parse(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc").unwrap_err();
        assert!(short.contains("shorter than its Content-Length"), "{short}");
        let mut head =
            read_response(&b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n"[..], "HEAD").unwrap();
        let mut body = Vec::new();
        head.body.read_to_end(&mut body).unwrap();
        assert!(body.is_empty());
        let accepted = parse(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert!(accepted.body.is_empty());
        let no_content = parse(b"HTTP/1.1 204 No Content\r\n\r\nignored").unwrap();
        assert!(no_content.body.is_empty());
        // A head that never ends is refused at the cap, not read forever.
        let endless = [b"HTTP/1.1 200 OK\r\nX: ".as_slice(), &[b'a'; 70_000]].concat();
        let long = parse(&endless).unwrap_err();
        assert!(long.contains("too long"), "{long}");
        // Bare LF line endings in the head are tolerated.
        let lf = parse(b"HTTP/1.1 200 OK\nX-A: b\n\nbody").unwrap();
        assert_eq!((lf.header("x-a"), lf.text().as_str()), (Some("b"), "body"));
    }

    /// The streaming reader hands over the first chunk while the server is
    /// still holding the rest back: the body is decoded as it arrives.
    #[test]
    fn request_streaming_yields_chunks_before_the_body_ends() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                head.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nfirst\r\n")
                .unwrap();
            stream.flush().unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            stream.write_all(b"6\r\nsecond\r\n0\r\n\r\n").unwrap();
        });
        let mut resp = request_streaming(
            "127.0.0.1",
            port,
            Request {
                method: "GET",
                path: "/stream",
                headers: &[],
                body: None,
            },
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(resp.status, 200);
        let mut first = [0u8; 5];
        resp.body.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"first");
        go.send(()).unwrap();
        let mut rest = String::new();
        resp.body.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "second");
        server.join().unwrap();
    }
}
