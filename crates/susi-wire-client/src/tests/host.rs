use super::*;
use std::io::{self, BufReader, Cursor, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn initialized() -> String {
    format!(
        "{}\n",
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":PROTOCOL_VERSION}})
    )
}

fn counted_streams(
    reader: impl BufRead + Send + 'static,
    closed: &Arc<AtomicUsize>,
) -> DuplexStreams {
    let closed = Arc::clone(closed);
    DuplexStreams {
        reader: Box::new(reader),
        writer: Box::new(Vec::<u8>::new()),
        on_close: Some(Box::new(move || {
            closed.fetch_add(1, Ordering::SeqCst);
        })),
    }
}

struct FailWrite {
    phase: usize,
    flushes: usize,
    on_flush: bool,
}
impl Write for FailWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.on_flush && self.flushes == self.phase {
            return Err(io::Error::other("scripted write failure"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.on_flush && self.flushes == self.phase {
            return Err(io::Error::other("scripted flush failure"));
        }
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn failed_handshakes_close_owned_streams_exactly_once() {
    let inputs = [
        String::new(),
        "not json\n".into(),
        "{\"id\":1,\"result\":{}}\n".into(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n".into(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"unknown\"}}\n".into(),
    ];
    for input in inputs {
        let closed = Arc::new(AtomicUsize::new(0));
        let mut host = McpHost::new();
        let streams = counted_streams(Cursor::new(input), &closed);
        assert!(host
            .connect_duplex("test", streams, HostPolicy::default())
            .is_err());
        assert!(host.server_ids().is_empty());
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        drop(host);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
    for (phase, on_flush) in [(0, false), (0, true), (1, false), (1, true)] {
        let closed = Arc::new(AtomicUsize::new(0));
        let mut host = McpHost::new();
        let mut streams = counted_streams(Cursor::new(initialized()), &closed);
        streams.writer = Box::new(FailWrite {
            phase,
            flushes: 0,
            on_flush,
        });
        let error = host
            .connect_duplex("test", streams, HostPolicy::default())
            .unwrap_err();
        assert!(matches!(error, WireError::Io(_)));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        drop(host);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn successful_peer_owns_cleanup_until_disconnect_or_drop_and_duplicate_is_cleaned() {
    for disconnect in [false, true] {
        let closed = Arc::new(AtomicUsize::new(0));
        let rejected = Arc::new(AtomicUsize::new(0));
        let mut host = McpHost::new();
        host.connect_duplex(
            "test",
            counted_streams(Cursor::new(initialized()), &closed),
            HostPolicy::default(),
        )
        .unwrap();
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        let error = host
            .connect_duplex(
                "test",
                counted_streams(Cursor::new(Vec::new()), &rejected),
                HostPolicy::default(),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "server `test` is already connected");
        assert_eq!(rejected.load(Ordering::SeqCst), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        if disconnect {
            host.disconnect("test").unwrap();
            assert_eq!(closed.load(Ordering::SeqCst), 1);
            assert!(host.disconnect("test").is_err());
        }
        drop(host);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(rejected.load(Ordering::SeqCst), 1);
    }
}

struct CountedRead<R> {
    inner: R,
    consumed: Arc<AtomicUsize>,
}
impl<R: BufRead> Read for CountedRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.fill_buf()?.read(bytes)?;
        self.consume(count);
        Ok(count)
    }
}
impl<R: BufRead> BufRead for CountedRead<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }
    fn consume(&mut self, count: usize) {
        self.consumed.fetch_add(count, Ordering::SeqCst);
        self.inner.consume(count);
    }
}
struct CountedWrite(Arc<AtomicUsize>);
impl Write for CountedWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.fetch_add(bytes.len(), Ordering::SeqCst);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn oversized_frames_are_bounded_and_poison_only_the_offending_peer() {
    let closed = Arc::new(AtomicUsize::new(0));
    let consumed = Arc::new(AtomicUsize::new(0));
    let written = Arc::new(AtomicUsize::new(0));
    let init = initialized();
    let body = io::repeat(b' ').take((MAX_DUPLEX_LINE_BYTES + 1) as u64);
    // The oversized prefix has no newline. A valid response and server callback
    // follow it; neither may be interpreted on a subsequent operation.
    let tail = Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"roots/list\"}\n");
    let reader = CountedRead {
        inner: Cursor::new(init.clone())
            .chain(BufReader::new(body))
            .chain(tail),
        consumed: Arc::clone(&consumed),
    };
    let mut streams = counted_streams(reader, &closed);
    streams.writer = Box::new(CountedWrite(Arc::clone(&written)));
    let mut host = McpHost::new();
    host.connect_duplex("bad", streams, HostPolicy::default())
        .unwrap();
    let error = host.list_tools("bad").unwrap_err();
    assert_eq!(error.to_string(), "duplex JSON-RPC line exceeded 16 MiB");
    assert_eq!(
        consumed.load(Ordering::SeqCst),
        init.len() + MAX_DUPLEX_LINE_BYTES + 1
    );
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    let prior_writes = written.load(Ordering::SeqCst);
    assert!(host.list_tools("bad").is_err());
    assert!(host.cancel_request("bad", json!(2), None).is_err());
    assert_eq!(written.load(Ordering::SeqCst), prior_writes);
    assert_eq!(
        consumed.load(Ordering::SeqCst),
        init.len() + MAX_DUPLEX_LINE_BYTES + 1
    );
    let other_closed = Arc::new(AtomicUsize::new(0));
    let other = init + "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n";
    host.connect_duplex(
        "good",
        counted_streams(Cursor::new(other), &other_closed),
        HostPolicy::default(),
    )
    .unwrap();
    assert!(host.list_tools("good").unwrap().is_empty());
    host.disconnect("bad").unwrap();
    drop(host);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(other_closed.load(Ordering::SeqCst), 1);
}

#[test]
fn oversized_unterminated_handshake_stops_acquisition_and_closes_once() {
    let closed = Arc::new(AtomicUsize::new(0));
    let consumed = Arc::new(AtomicUsize::new(0));
    let reader = CountedRead {
        inner: BufReader::new(io::repeat(b'x').take((MAX_DUPLEX_LINE_BYTES * 3) as u64)),
        consumed: Arc::clone(&consumed),
    };
    let mut host = McpHost::new();
    let error = host
        .connect_duplex(
            "test",
            counted_streams(reader, &closed),
            HostPolicy::default(),
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "duplex JSON-RPC line exceeded 16 MiB");
    assert_eq!(consumed.load(Ordering::SeqCst), MAX_DUPLEX_LINE_BYTES + 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    drop(host);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[test]
fn line_limit_preserves_exact_boundary_eof_and_unicode_whitespace() {
    for (length, newline, accepted) in [
        (MAX_DUPLEX_LINE_BYTES, true, true),
        (MAX_DUPLEX_LINE_BYTES, false, true),
        (MAX_DUPLEX_LINE_BYTES + 1, true, false),
    ] {
        let mut frame = format!("\u{2003}{}", initialized().trim_end()).into_bytes();
        frame.resize(length, b' ');
        if newline {
            *frame.last_mut().unwrap() = b'\n';
        }
        let closed = Arc::new(AtomicUsize::new(0));
        let mut host = McpHost::new();
        let result = host.connect_duplex(
            "test",
            counted_streams(Cursor::new(frame), &closed),
            HostPolicy::default(),
        );
        assert_eq!(
            result.is_ok(),
            accepted,
            "length={length}, newline={newline}: {result:?}"
        );
        drop(host);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}

struct InvalidRead;
impl Read for InvalidRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reader rejected input",
        ))
    }
}
impl BufRead for InvalidRead {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reader rejected input",
        ))
    }
    fn consume(&mut self, _: usize) {}
}

#[test]
fn ordinary_rpc_and_input_errors_keep_their_existing_semantics() {
    let closed = Arc::new(AtomicUsize::new(0));
    let input = initialized() + "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32601,\"message\":\"absent\"}}\n{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[]}}\n";
    let mut host = McpHost::new();
    host.connect_duplex(
        "test",
        counted_streams(Cursor::new(input), &closed),
        HostPolicy::default(),
    )
    .unwrap();
    assert!(matches!(
        host.list_tools("test"),
        Err(WireError::Rpc(-32601, _))
    ));
    assert!(host.list_tools("test").unwrap().is_empty());
    assert_eq!(closed.load(Ordering::SeqCst), 0);
    for (reader, message) in [
        (
            Box::new(Cursor::new(vec![0xff, b'\n'])) as Box<dyn BufRead + Send>,
            "stream did not contain valid UTF-8",
        ),
        (Box::new(InvalidRead), "reader rejected input"),
    ] {
        let mut streams = counted_streams(Cursor::new(Vec::new()), &closed);
        streams.reader = reader;
        let error = host
            .connect_duplex("invalid", streams, HostPolicy::default())
            .unwrap_err();
        assert!(matches!(error, WireError::Io(ref text) if text == message));
    }
    drop(host);
    assert_eq!(closed.load(Ordering::SeqCst), 3);
}

struct ChildOwner(Arc<Mutex<Child>>);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[test]
fn duplex_child_fixture() {
    if std::env::var_os("SUSI_WIRE_TEST_CHILD").is_none() {
        return;
    }
    let mut output = io::stdout().lock();
    writeln!(output, "MCP-FIXTURE-READY").unwrap();
    output.flush().unwrap();
    let mut input = io::stdin().lock();
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    assert!(line.contains("initialize"));
    writeln!(output, "malformed initialize reply").unwrap();
    output.flush().unwrap();
    // Remain alive until the owner's cleanup kills and reaps this direct child.
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
    }
}

#[test]
fn failed_handshake_kills_and_reaps_the_actual_owned_child() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host::tests::duplex_child_fixture",
            "--nocapture",
        ])
        .env("SUSI_WIRE_TEST_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let writer = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let child = Arc::new(Mutex::new(child));
    let _fallback = ChildOwner(Arc::clone(&child));
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.trim() == "MCP-FIXTURE-READY" {
            break;
        }
    }
    assert!(child.lock().unwrap().try_wait().unwrap().is_none());
    let cleanup_child = Arc::clone(&child);
    let closed = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&closed);
    let mut host = McpHost::new();
    let result = host.connect_duplex(
        "child",
        DuplexStreams {
            reader: Box::new(reader),
            writer: Box::new(writer),
            on_close: Some(Box::new(move || {
                let mut child = cleanup_child.lock().unwrap();
                assert!(child.try_wait().unwrap().is_none());
                child.kill().unwrap();
                child.wait().unwrap();
                count.fetch_add(1, Ordering::SeqCst);
            })),
        },
        HostPolicy::default(),
    );
    assert!(
        matches!(result, Err(WireError::Protocol(ref message)) if message.starts_with("invalid JSON-RPC frame:"))
    );
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert!(child.lock().unwrap().try_wait().unwrap().is_some());
    drop(host);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}
