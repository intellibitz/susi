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
