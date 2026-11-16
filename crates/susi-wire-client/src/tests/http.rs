use super::*;

#[test]
fn parses_length_and_chunked_bodies() {
    let plain = parse(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello").unwrap();
    assert_eq!((plain.status, plain.text().as_str()), (200, "hello"));
    assert_eq!(plain.header("CONTENT-TYPE"), Some("text/plain"));
    let chunked = parse(
        b"HTTP/1.1 401 X\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n",
    )
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
