use super::*;

fn decode<const PEER: bool>(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    Body::<_, PEER>::chunked(raw).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[test]
fn clients_keep_their_distinct_chunk_errors() {
    for (raw, wire_message, peer_kind, peer_message) in [
        (
            &b"zz\r\n"[..],
            "invalid chunk size \"zz\"",
            io::ErrorKind::InvalidData,
            "invalid chunk size",
        ),
        (
            &b"3\r\na"[..],
            "truncated chunk",
            io::ErrorKind::UnexpectedEof,
            "truncated chunk",
        ),
        (
            &b"1\r\na"[..],
            "missing chunk terminator",
            io::ErrorKind::UnexpectedEof,
            "failed to fill whole buffer",
        ),
    ] {
        let wire = decode::<false>(raw).unwrap_err();
        assert_eq!(
            (wire.kind(), wire.to_string()),
            (io::ErrorKind::InvalidData, wire_message.into())
        );
        let peer = decode::<true>(raw).unwrap_err();
        assert_eq!(
            (peer.kind(), peer.to_string()),
            (peer_kind, peer_message.into())
        );
    }
}

#[test]
fn chunk_extensions_trailers_empty_reads_and_final_eof_stay_compatible() {
    for raw in [
        &b"1;x=1\r\na\r\n0\r\n"[..],
        &b"1\r\na\r\n0\r\nX-T: yes\r\n\r\n"[..],
    ] {
        assert_eq!(decode::<false>(raw).unwrap(), b"a");
        assert_eq!(decode::<true>(raw).unwrap(), b"a");
    }
    let mut body = Body::<_, false>::chunked(&b"1\r\na\r\n0\r\n\r\n"[..]);
    assert_eq!(body.read(&mut []).unwrap(), 0);
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"a");
    assert_eq!(body.read(&mut [0]).unwrap(), 0);
}

#[test]
fn timeout_at_chunk_terminator_is_never_reclassified() {
    struct Timeout;
    impl Read for Timeout {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "request budget"))
        }
    }
    impl BufRead for Timeout {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "request budget"))
        }
        fn consume(&mut self, _: usize) {}
    }
    fn check<const PEER: bool>() {
        let reader = (&b"1\r\na"[..]).chain(Timeout);
        let error = Body::<_, PEER>::chunked(reader).read(&mut [0]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "request budget");
    }
    check::<false>();
    check::<true>();
}

#[test]
fn response_heads_retain_client_status_policies() {
    for (line, wire, peer) in [
        ("HTTP/1.1 200 OK", Some(200), Some(200)),
        ("HTTP/1.extra 599 End", Some(599), Some(599)),
        ("HTTP/2 200 OK", Some(200), None),
        ("other 0", Some(0), None),
        ("HTTP/1.1 600", Some(600), None),
        ("HTTP/1.1 +200", Some(200), None),
        ("HTTP/1.1 65535", Some(65535), None),
        ("HTTP/1.1 65536", None, None),
        ("HTTP/1.1\t201", Some(201), Some(201)),
        ("\u{2003}HTTP/1.0\u{2003}202", Some(202), Some(202)),
        ("HTTP/1.1", None, None),
    ] {
        let raw = format!("{line}\r\n\r\nbody");
        let mut input = raw.as_bytes();
        assert_eq!(
            read_response_head::<_, false>(&mut input).ok().map(|v| v.0),
            wire
        );
        assert_eq!(input, b"body");
        let mut input = raw.as_bytes();
        assert_eq!(
            read_response_head::<_, true>(&mut input).ok().map(|v| v.0),
            peer
        );
        assert_eq!(input, b"body");
    }
}

#[test]
fn response_heads_retain_lossy_headers_order_and_first_head_only() {
    let raw = b"HTTP/1.1 100 Continue\r\n X-A : one \r\nX-A: two:three\ninvalid\nX-B: \xff\r\r\n\r\nHTTP/1.1 200 OK\r\n\r\n";
    fn check<const PEER: bool>(mut input: &[u8]) {
        let head = read_response_head::<_, PEER>(&mut input).unwrap();
        assert_eq!(head.0, 100);
        assert_eq!(
            head.1,
            vec![
                ("x-a".into(), "one".into()),
                ("x-a".into(), "two:three".into()),
                ("x-b".into(), "\u{fffd}".into())
            ]
        );
        assert_eq!(input, b"HTTP/1.1 200 OK\r\n\r\n");
    }
    check::<false>(raw);
    check::<true>(raw);
}

#[test]
fn response_head_errors_keep_kinds_messages_and_consumption() {
    use io::ErrorKind::{InvalidData, UnexpectedEof};
    for (raw, wire, peer) in [
        (
            &b""[..],
            (InvalidData, "response has no header terminator"),
            (
                UnexpectedEof,
                "connection closed before a complete response head",
            ),
        ),
        (
            &b"HTTP/1.1 200 OK\r\n"[..],
            (InvalidData, "response has no header terminator"),
            (
                UnexpectedEof,
                "connection closed before a complete response head",
            ),
        ),
        (
            &b"HTTP/1.1 200"[..],
            (InvalidData, "response has no header terminator"),
            (InvalidData, "response has no header terminator"),
        ),
        (
            &b"\r\nbody"[..],
            (InvalidData, "response has no status line"),
            (InvalidData, "response has no status line"),
        ),
        (
            &b"HTTP/2 200\n\nbody"[..],
            (InvalidData, "unused"),
            (InvalidData, "response is not HTTP/1.x"),
        ),
        (
            &b"HTTP/1.1 nope\n\nbody"[..],
            (InvalidData, "response has no status line"),
            (InvalidData, "response has no valid status code"),
        ),
    ] {
        let mut input = raw;
        let result = read_response_head::<_, false>(&mut input);
        if wire.1 != "unused" {
            let error = result.unwrap_err();
            assert_eq!((error.kind(), error.to_string()), (wire.0, wire.1.into()));
        } else {
            assert_eq!(result.unwrap().0, 200);
        }
        let remainder = input;
        let mut input = raw;
        let error = read_response_head::<_, true>(&mut input).unwrap_err();
        assert_eq!((error.kind(), error.to_string()), (peer.0, peer.1.into()));
        assert_eq!(input, remainder);
    }
}

#[test]
fn response_head_limit_includes_the_terminator_and_leaves_the_body() {
    fn check<const PEER: bool>() {
        for extra in [0, 1] {
            let mut raw = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
            raw.resize(MAX_HEAD_BYTES as usize - 4 + extra, b'a');
            raw.extend_from_slice(b"\r\n\r\nbody");
            let mut input = raw.as_slice();
            let result = read_response_head::<_, PEER>(&mut input);
            if extra == 0 {
                assert_eq!(result.unwrap().0, 200);
                assert_eq!(input, b"body");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert_eq!(error.to_string(), "response line too long");
                assert_eq!(input, b"\nbody");
            }
        }
    }
    check::<false>();
    check::<true>();
}
