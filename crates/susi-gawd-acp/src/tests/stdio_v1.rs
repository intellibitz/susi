use super::*;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;

include!("session_factory.rs");

struct Client {
    input: TcpStream,
    output: BufReader<TcpStream>,
    worker: Option<thread::JoinHandle<io::Result<usize>>>,
}
impl Client {
    fn start(engine: Arc<AcpEngine>) -> Self {
        Self::start_with(engine, |stream| stream)
    }
    fn start_with<W: Write + Send + 'static>(
        engine: Arc<AcpEngine>,
        writer: impl FnOnce(TcpStream) -> W + Send + 'static,
    ) -> Self {
        Self::start_io(engine, BufReader::new, writer)
    }
    fn start_io<R: BufRead + Send + 'static, W: Write + Send + 'static>(
        engine: Arc<AcpEngine>,
        reader: impl FnOnce(TcpStream) -> R + Send + 'static,
        writer: impl FnOnce(TcpStream) -> W + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let input = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (server, _) = listener.accept().unwrap();
        let worker = thread::spawn(move || {
            AcpStdioTransport::serve_negotiated_reader_writer(
                &engine,
                reader(server.try_clone().unwrap()),
                writer(server),
            )
        });
        Self {
            output: BufReader::new(input.try_clone().unwrap()),
            input,
            worker: Some(worker),
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
        self.read()
    }
    fn initialize(&mut self) -> Value {
        self.request(
            1,
            "initialize",
            json!({"protocolVersion":1, "clientCapabilities":{}}),
        )
    }
    fn session(&mut self) -> Value {
        self.request(
            2,
            "session/new",
            json!({"cwd":std::env::current_dir().unwrap(), "mcpServers":[]}),
        )
    }
    fn close(mut self) -> usize {
        self.input.shutdown(Shutdown::Write).unwrap();
        self.worker.take().unwrap().join().unwrap().unwrap()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.input.shutdown(Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
fn v1_wire_shapes_preserve_references_and_workspace_without_fetching() {
    let engine = Arc::new(AcpEngine::default());
    let mut client = Client::start(Arc::clone(&engine));
    let init = client.initialize();
    assert_eq!(init["result"]["protocolVersion"], 1);
    assert!(init["result"].get("protocol_version").is_none());
    assert_eq!(init["result"]["agentCapabilities"]["loadSession"], false);
    let created = client.session();
    let sid = created["result"]["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(
        engine.state.lock().unwrap().sessions[&sid]
            .summary
            .workspace,
        Some(
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
    let resource = json!({"type":"resource_link", "name":"private reference", "uri":"https://127.0.0.1:1/no-fetch", "mimeType":"text/plain"});
    let params = json!({"sessionId":sid,"prompt":[{"type":"text","text":"explain reference"},resource.clone()]});
    client.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":params}));
    let update = client.read();
    assert_eq!(update["method"], "session/update");
    assert!(update.get("id").is_none());
    assert_eq!(
        update["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    let text = update["params"]["update"]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("not fetched") && text.contains("https://127.0.0.1:1/no-fetch"));
    assert!(text.contains("private reference") && text.contains("text/plain"));
    let done = client.read();
    assert_eq!(
        done,
        json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}})
    );
    assert_eq!(client.close(), 3);
    if let Some(path) = std::env::var_os("SUSI_ACP_V1_SCHEMA_TRANSCRIPT") {
        // Opt-in audit evidence only: validate these actual wire values against
        // the external pinned schema, without importing it into this repository.
        let samples = json!([
            {"definition":"InitializeResponse","value":init["result"]},
            {"definition":"NewSessionResponse","value":created["result"]},
            {"definition":"PromptRequest","value":params},
            {"definition":"ContentBlock","value":resource},
            {"definition":"SessionNotification","value":update["params"]},
            {"definition":"PromptResponse","value":done["result"]}
        ]);
        std::fs::write(path, serde_json::to_vec_pretty(&samples).unwrap()).unwrap();
    }
}

#[test]
fn v1_refuses_unsupported_inputs_before_creating_or_running_a_session() {
    let engine = Arc::new(AcpEngine::default());
    let mut client = Client::start(Arc::clone(&engine));
    assert_eq!(
        client.request(1, "initialize", json!({"protocolVersion":999}))["result"]
            ["protocolVersion"],
        1
    );
    for params in [
        json!({"cwd":"relative","mcpServers":[]}),
        json!({"cwd":"/tmp"}),
        json!({"cwd":"/tmp","mcpServers":[{"name":"m","command":"/bin/no-run","args":[],"env":[]}]}),
    ] {
        assert!(client
            .request(2, "session/new", params)
            .get("error")
            .is_some());
        assert!(engine.state.lock().unwrap().sessions.is_empty());
    }
    let sid = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    for block in [
        json!({"type":"resource_link","uri":"file:///x"}),
        json!({"type":"image","data":"x","mimeType":"image/png"}),
    ] {
        assert_eq!(
            client.request(
                3,
                "session/prompt",
                json!({"sessionId":sid,"prompt":[block]})
            )["error"]["code"],
            JSONRPC_INVALID_PARAMS
        );
        assert_eq!(
            engine.state.lock().unwrap().sessions[&sid]
                .summary
                .turns_completed,
            0
        );
    }
    // Unsupported notifications have no response; the next response is the request's.
    client.send(json!({"jsonrpc":"2.0","method":"_unknown/notification","params":{}}));
    client.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":[]}));
    client.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":42}}));
    assert_eq!(client.request(9, "session/list", json!({}))["id"], 9);
    client.close();
}

#[test]
fn v1_preserves_exact_workspace_and_opaque_session_identity() {
    let engine = Arc::new(AcpEngine::default());
    let mut client = Client::start(Arc::clone(&engine));
    client.initialize();
    let cwd = std::env::current_dir()
        .unwrap()
        .join("workspace ending in a space ");
    let reply = client.request(2, "session/new", json!({"cwd":cwd,"mcpServers":[]}));
    let sid = reply["result"]["sessionId"].as_str().unwrap();
    assert_eq!(
        engine.state.lock().unwrap().sessions[sid]
            .summary
            .workspace
            .as_deref(),
        cwd.to_str()
    );
    let reply = client.request(
        3,
        "session/prompt",
        json!({"sessionId":format!(" {sid} "),"prompt":[{"type":"text","text":"no alias"}]}),
    );
    assert_eq!(reply["error"]["code"], -32002);
    assert_eq!(
        engine.state.lock().unwrap().sessions[sid]
            .summary
            .turns_completed,
        0
    );
    client.close();
}

struct Blocking {
    entered: mpsc::Sender<()>,
}
impl AcpTurnExecutor for Blocking {
    fn execute_turn(
        &self,
        _: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        emit("running");
        self.entered.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            cancelled.load(Ordering::SeqCst),
            "transport must deliver cancellation while prompt is running"
        );
        emit("must not escape cancellation");
        Ok(Vec::new())
    }
}

#[test]
fn v1_cancel_is_one_way_concurrent_and_does_not_arm_the_next_turn() {
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy {
            max_sessions: 1,
            ..Default::default()
        },
        Arc::new(Blocking { entered: tx }),
    ));
    let mut client = Client::start(Arc::clone(&engine));
    client.initialize();
    let sid = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    for id in [3, 5] {
        client.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"wait"}]}}));
        assert_eq!(client.read()["method"], "session/update");
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            client.session()["error"]["code"],
            ACP_ERR_CAPABILITY_REFUSED
        );
        assert_eq!(engine.state.lock().unwrap().sessions.len(), 1);
        // A concurrent turn on the same session is refused, not allowed to
        // overwrite the engine's cancellation ownership.
        assert_eq!(
            client.request(
                id + 1,
                "session/prompt",
                json!({"sessionId":sid,"prompt":[{"type":"text","text":"overlap"}]})
            )["error"]["code"],
            ACP_ERR_CAPABILITY_REFUSED
        );
        client.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}}));
        assert_eq!(
            client.read(),
            json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"cancelled"}})
        );
        assert_eq!(
            client.request(id + 10, "session/list", json!({}))["id"],
            id + 10
        );
    }
    client.close();
    let state = engine.state.lock().unwrap();
    assert!(state.active_cancellations.is_empty() && state.turn_cancellations.is_empty());
}

#[test]
fn v1_eof_cancels_and_reaps_cooperative_worker_without_late_output() {
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(Blocking { entered: tx }),
    ));
    let mut client = Client::start(Arc::clone(&engine));
    client.initialize();
    let sid = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    client.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"wait"}]}}));
    client.read();
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    client.input.shutdown(Shutdown::Write).unwrap();
    let mut remaining = String::new();
    client.output.read_to_string(&mut remaining).unwrap();
    assert!(
        remaining.is_empty(),
        "no output after input EOF: {remaining}"
    );
    assert_eq!(client.worker.take().unwrap().join().unwrap().unwrap(), 3);
    assert!(engine.state.lock().unwrap().active_cancellations.is_empty());
}

#[test]
fn negotiated_transport_preserves_legacy_wire_and_non_send_api() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocol_version\":\"2026-11-11\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/prompt\",\"params\":{\"session_id\":\"acp-sess-000001\",\"prompt\":\"hello\"}}\n"
    );
    let mut old = Vec::new();
    let mut new = Vec::new();
    AcpStdioTransport::serve_reader_writer(&AcpEngine::default(), io::Cursor::new(input), &mut old)
        .unwrap();
    AcpStdioTransport::serve_negotiated_reader_writer(
        &AcpEngine::default(),
        io::Cursor::new(input),
        &mut new,
    )
    .unwrap();
    let old: Vec<Value> = String::from_utf8(old)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let new: Vec<Value> = String::from_utf8(new)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(old[0], new[0]);
    assert_eq!(
        old[1]["result"]["session_id"],
        new[1]["result"]["session_id"]
    );
    assert_eq!(old[2]["result"]["output"], new[2]["result"]["output"]);
    assert_eq!(
        old[2]["result"]["stop_reason"],
        new[2]["result"]["stop_reason"]
    );
    struct Local(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl Write for Local {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    AcpStdioTransport::serve_reader_writer(
        &AcpEngine::default(),
        io::Cursor::new(input),
        Local(Default::default()),
    )
    .unwrap();
}

#[test]
fn oversized_frame_is_bounded_and_its_tail_is_not_a_request() {
    for suffix in [
        Vec::new(),
        b"\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\"params\":{}}\n".to_vec(),
    ] {
        let mut bytes = vec![b'x'; MAX_STDIO_LINE_BYTES * 3];
        bytes.extend(suffix);
        let mut reader = io::Cursor::new(bytes);
        let mut output = Vec::new();
        let engine = AcpEngine::default();
        let error =
            AcpStdioTransport::serve_negotiated_reader_writer(&engine, &mut reader, &mut output)
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(reader.position() <= (MAX_STDIO_LINE_BYTES + 2) as u64);
        let reply: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["error"]["code"], JSONRPC_INVALID_REQUEST);
        assert!(engine.state.lock().unwrap().sessions.is_empty());
    }
}

struct FinalOutput {
    deltas: Vec<String>,
    blocks: Vec<AcpContentBlock>,
}
impl AcpTurnExecutor for FinalOutput {
    fn execute_turn(
        &self,
        _: &AcpTurnContext,
        _: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        for delta in &self.deltas {
            emit(delta);
        }
        Ok(self.blocks.clone())
    }
}

#[test]
fn canonical_output_is_preserved_for_empty_partial_full_and_divergent_streams() {
    type Case<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str]);
    let cases: &[Case<'_>] = &[
        (&[], &["answer"], &["answer"]),
        (&[""], &["answer"], &["answer"]),
        (&["prefix"], &["prefix rest"], &["prefix rest"]),
        (&["answer"], &["answer"], &["answer"]),
        (&["one"], &["one", "two"], &["one\ntwo"]),
        (&["draft"], &["answer"], &["draft", "answer"]),
    ];
    for &(deltas, blocks, expected) in cases {
        let engine = Arc::new(AcpEngine::new(
            AcpAdmissionPolicy::default(),
            Arc::new(FinalOutput {
                deltas: deltas.iter().map(|s| (*s).to_owned()).collect(),
                blocks: blocks.iter().map(|s| AcpContentBlock::text(*s)).collect(),
            }),
        ));
        let mut client = Client::start(engine);
        client.initialize();
        let sid = client.session()["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        client.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"run"}]}}));
        let mut messages = BTreeMap::<String, String>::new();
        loop {
            let reply = client.read();
            if reply.get("id").is_some() {
                assert_eq!(reply["result"]["stopReason"], "end_turn");
                break;
            }
            let update = &reply["params"]["update"];
            messages
                .entry(update["messageId"].as_str().unwrap().into())
                .or_default()
                .push_str(update["content"]["text"].as_str().unwrap());
        }
        assert_eq!(
            messages.values().map(String::as_str).collect::<Vec<_>>(),
            expected,
            "deltas={deltas:?}, final={blocks:?}"
        );
        client.close();
    }
}

struct Gate {
    released: Mutex<bool>,
    wake: std::sync::Condvar,
    entered: mpsc::Sender<()>,
    once: AtomicBool,
}
impl Gate {
    fn wait_once(&self) {
        if !self.once.swap(true, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct ReleaseGate(Arc<Gate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct GatedWriter {
    stream: TcpStream,
    gate: Arc<Gate>,
}
impl Write for GatedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let line = String::from_utf8_lossy(bytes);
        if line.contains("\"id\":20") && line.contains("\"stopReason\"") {
            self.gate.wait_once();
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
struct BackpressureExecutor {
    cancelled: mpsc::Sender<()>,
}
impl AcpTurnExecutor for BackpressureExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        if ctx.joined_prompt_text() == "A" {
            return Ok(vec![AcpContentBlock::text("A")]);
        }
        emit("B running");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !cancelled.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        if cancelled.load(Ordering::SeqCst) {
            self.cancelled.send(()).unwrap();
        }
        Ok(Vec::new())
    }
}

#[test]
fn blocked_terminal_output_does_not_prevent_other_turn_cancellation_or_eof_signalling() {
    for eof in [false, true] {
        let (entered, entered_rx) = mpsc::channel();
        let (cancelled, cancelled_rx) = mpsc::channel();
        let gate = Arc::new(Gate {
            released: Mutex::new(false),
            wake: Default::default(),
            entered,
            once: AtomicBool::new(false),
        });
        let writer_gate = Arc::clone(&gate);
        let engine = Arc::new(AcpEngine::new(
            AcpAdmissionPolicy::default(),
            Arc::new(BackpressureExecutor { cancelled }),
        ));
        let mut client = Client::start_with(engine, move |stream| GatedWriter {
            stream,
            gate: writer_gate,
        });
        // Drop order releases the writer before Client's join even on assertion failure.
        let release = ReleaseGate(Arc::clone(&gate));
        client.initialize();
        let a = client.session()["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        let b = client.session()["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        for (id, sid, text) in [(10, &b, "B"), (20, &a, "A")] {
            client.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":text}]}}));
            assert_eq!(client.read()["method"], "session/update");
        }
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        if eof {
            client.input.shutdown(Shutdown::Write).unwrap();
        } else {
            client
                .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":b}}));
        }
        let signalled_before_release = cancelled_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        drop(release);
        // A's write was already in flight; generic Write cannot revoke it.
        assert_eq!(client.read()["id"], 20);
        if eof {
            let mut tail = String::new();
            client.output.read_to_string(&mut tail).unwrap();
            assert!(tail.is_empty(), "no new frames after EOF: {tail}");
            client.worker.take().unwrap().join().unwrap().unwrap();
        } else {
            assert_eq!(
                client.read(),
                json!({"jsonrpc":"2.0","id":10,"result":{"stopReason":"cancelled"}})
            );
            client.close();
        }
        assert!(
            signalled_before_release,
            "eof={eof}: cancellation must not wait for blocked output"
        );
    }
}

struct FailingWriter {
    stream: TcpStream,
    failed: mpsc::Sender<()>,
    gate: Arc<Gate>,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let line = String::from_utf8_lossy(bytes);
        if line.contains("\"id\":20") && line.contains("\"stopReason\"") {
            self.gate.wait_once();
            self.failed.send(()).unwrap();
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "peer output disconnected",
            ));
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

struct GatedReader {
    inner: BufReader<TcpStream>,
    frames: usize,
    gate: Arc<Gate>,
}
impl Read for GatedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.fill_buf()?.read(bytes)?;
        self.consume(count);
        Ok(count)
    }
}
impl BufRead for GatedReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.frames == 0 {
            self.gate.wait_once();
        }
        self.inner.fill_buf()
    }
    fn consume(&mut self, count: usize) {
        let frames = self.inner.buffer()[..count]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        self.frames = self.frames.saturating_sub(frames);
        self.inner.consume(count);
    }
}
struct ObservedExecutor {
    inner: BackpressureExecutor,
    entered: mpsc::Sender<String>,
}
impl AcpTurnExecutor for ObservedExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        self.entered.send(ctx.joined_prompt_text()).unwrap();
        self.inner.execute_turn(ctx, cancelled, emit)
    }
}

#[test]
fn output_failure_cancels_idle_turns_and_refuses_queued_prompts() {
    let (failed, failed_rx) = mpsc::channel();
    let (cancelled, cancelled_rx) = mpsc::channel();
    let (executed, executed_rx) = mpsc::channel();
    let (reader_entered, reader_rx) = mpsc::channel();
    let (writer_entered, writer_rx) = mpsc::channel();
    let reader_gate = Arc::new(Gate {
        released: Mutex::new(false),
        wake: Default::default(),
        entered: reader_entered,
        once: AtomicBool::new(false),
    });
    let writer_gate = Arc::new(Gate {
        released: Mutex::new(false),
        wake: Default::default(),
        entered: writer_entered,
        once: AtomicBool::new(false),
    });
    let reader_clone = Arc::clone(&reader_gate);
    let writer_clone = Arc::clone(&writer_gate);
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(ObservedExecutor {
            inner: BackpressureExecutor { cancelled },
            entered: executed,
        }),
    ));
    let mut client = Client::start_io(
        Arc::clone(&engine),
        move |stream| GatedReader {
            inner: BufReader::new(stream),
            frames: 6, // initialize, three sessions, then prompts B and A
            gate: reader_clone,
        },
        move |stream| FailingWriter {
            stream,
            failed,
            gate: writer_clone,
        },
    );
    let release_reader = ReleaseGate(reader_gate);
    let release_writer = ReleaseGate(writer_gate);
    client.initialize();
    let a = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let c = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    for (id, sid, text) in [(10, &b, "B"), (20, &a, "A")] {
        client.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":text}]}}));
        assert_eq!(client.read()["method"], "session/update");
        assert_eq!(
            executed_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            text
        );
    }
    writer_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    reader_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    // The reader is past its finished-worker sweep and cannot observe another
    // frame while A's output fails and B receives cancellation.
    drop(release_writer);
    failed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let cancelled_with_input_open = cancelled_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    client.send(json!({"jsonrpc":"2.0","id":30,"method":"session/prompt","params":{"sessionId":c,"prompt":[{"type":"text","text":"C"}]}}));
    drop(release_reader);
    client.input.shutdown(Shutdown::Write).unwrap();
    let error = client.worker.take().unwrap().join().unwrap().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(
        executed_rx.try_recv().is_err(),
        "closed output must prevent queued C execution"
    );
    let state = engine.state.lock().unwrap();
    assert!(
        !state.sessions.contains_key(&c),
        "closed output removes connection-owned sessions; queued C still never executes"
    );
    assert!(
        cancelled_with_input_open,
        "output failure must signal all connection turns before another read"
    );
}
