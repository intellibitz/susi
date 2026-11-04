//! Minimal HTTP/1.1 client over `std::net`, enough to exercise the host
//! contract without pulling a client stack into the suite.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

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
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The body as text (lossy).
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
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
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
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
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read from {host}:{port}: {e}"))?;
    parse(&raw)
}

fn parse(raw: &[u8]) -> Result<Response, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("response has no header terminator")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("response has no status line")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let rest = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        dechunk(rest)?
    } else {
        rest.to_vec()
    };
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("truncated chunk size")?;
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| format!("invalid chunk size {size_text:?}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&data[..size]);
        if data.get(size..size + 2) != Some(b"\r\n") {
            return Err("missing chunk terminator".into());
        }
        data = &data[size + 2..];
    }
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
}
