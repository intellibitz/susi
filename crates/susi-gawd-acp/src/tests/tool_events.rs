enum TypedOutcome {
    Complete,
    Fail,
    Cancel(mpsc::Sender<()>),
}
struct TypedExecutor(TypedOutcome);
impl AcpTurnExecutor for TypedExecutor {
    fn execute_turn(
        &self,
        _: &AcpTurnContext,
        _: &AtomicBool,
        _: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        panic!("typed override must be called")
    }
    fn execute_turn_with_events(
        &self,
        ctx: &AcpTurnContext,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(AcpExecutorEvent),
    ) -> Result<AcpExecutorOutput, String> {
        let id = format!("{}-call-1", ctx.turn_id);
        let mut start = AcpToolCall::new(&id, "Calling fixture tool");
        start.name = Some("fixture_tool".into());
        emit(AcpExecutorEvent::ToolCall(start));
        let mut update = AcpToolCallUpdate::new(&id);
        update.status = Some(AcpToolCallStatus::InProgress);
        emit(AcpExecutorEvent::ToolCallUpdate(update));
        match &self.0 {
            TypedOutcome::Complete => {
                for (status, text) in [
                    (None, "first content"),
                    (Some(AcpToolCallStatus::Completed), "replacement content"),
                ] {
                    let mut update = AcpToolCallUpdate::new(&id);
                    update.status = status;
                    update.content = Some(vec![AcpToolCallContent::text(text)]);
                    emit(AcpExecutorEvent::ToolCallUpdate(update));
                }
                emit(AcpExecutorEvent::MessageDelta("answer".into()));
                Ok(AcpExecutorOutput {
                    blocks: vec![AcpContentBlock::text("answer")],
                    tool_calls: Vec::new(),
                })
            }
            TypedOutcome::Fail => {
                emit(AcpExecutorEvent::ToolCallUpdate(failed_tool_update(
                    &id,
                    "MCP tool failed",
                )));
                Err("MCP tool failed".into())
            }
            TypedOutcome::Cancel(observed) => {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !cancelled.load(Ordering::SeqCst)
                    && std::time::Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(2));
                }
                assert!(cancelled.load(Ordering::SeqCst), "cancel was not delivered");
                observed.send(()).unwrap();
                emit(AcpExecutorEvent::ToolCallUpdate(failed_tool_update(
                    &id,
                    "Cancelled",
                )));
                emit(AcpExecutorEvent::MessageDelta("must be suppressed".into()));
                Err("aborted operation".into())
            }
        }
    }
}

fn failed_tool_update(id: &str, message: &str) -> AcpToolCallUpdate {
    let mut update = AcpToolCallUpdate::new(id);
    update.status = Some(AcpToolCallStatus::Failed);
    update.content = Some(vec![AcpToolCallContent::text(message)]);
    update
}

fn typed_client(outcome: TypedOutcome) -> (Arc<AcpEngine>, Client, String) {
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(TypedExecutor(outcome)),
    ));
    let mut client = Client::start(Arc::clone(&engine));
    client.initialize();
    let session = client.session()["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    (engine, client, session)
}

fn send_typed_prompt(client: &mut Client, session: &str, id: u64) {
    client.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{
        "sessionId":session,"prompt":[{"type":"text","text":"call the tool"}]
    }}));
}

fn export_tool_wire(name: &str, frames: &[Value]) {
    if let Some(directory) = std::env::var_os("SUSI_ACP_TOOL_SCHEMA_TRANSCRIPTS") {
        // Audit-only: these are values read from the actual transport socket.
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec_pretty(frames).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn typed_tool_events_reach_v1_in_order_with_replacement_content_and_unique_turn_ids() {
    let (engine, mut client, sid) = typed_client(TypedOutcome::Complete);
    let mut transcript = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for request_id in [3, 4] {
        send_typed_prompt(&mut client, &sid, request_id);
        let frames: Vec<Value> = (0..6).map(|_| client.read()).collect();
        let calls: Vec<_> = frames[..4]
            .iter()
            .map(|frame| &frame["params"]["update"])
            .collect();
        assert_eq!(calls[0]["sessionUpdate"], "tool_call");
        assert_eq!(calls[0]["title"], "Calling fixture tool");
        assert_eq!(calls[0]["name"], "fixture_tool");
        assert_eq!(calls[0]["status"], "pending");
        assert!(ids.insert(calls[0]["toolCallId"].as_str().unwrap().to_owned()));
        for frame in &frames[..5] {
            assert_eq!(frame["jsonrpc"], "2.0");
            assert_eq!(frame["method"], "session/update");
            assert!(frame.get("id").is_none());
            assert_eq!(frame["params"]["sessionId"], sid);
        }
        for call in &calls[1..] {
            assert_eq!(call["sessionUpdate"], "tool_call_update");
            assert_eq!(call["toolCallId"], calls[0]["toolCallId"]);
        }
        assert_eq!(calls[1]["status"], "in_progress");
        assert!(calls[1].get("content").is_none());
        assert!(calls[2].get("status").is_none());
        assert_eq!(calls[3]["status"], "completed");
        for (call, text) in [(calls[2], "first content"), (calls[3], "replacement content")] {
            assert_eq!(call["content"], json!([{"type":"content","content":{"type":"text","text":text}}]));
        }
        assert_eq!(frames[4]["params"]["update"]["sessionUpdate"], "agent_message_chunk");
        assert_eq!(frames[4]["params"]["update"]["content"]["text"], "answer");
        assert_eq!(frames[5], rpc_ok(json!(request_id), json!({"stopReason":"end_turn"})));
        transcript.extend(frames);
    }
    let history = engine.dispatch_session_history(&json!({"session_id":sid})).unwrap();
    assert_eq!(history["messages"].as_array().unwrap().len(), 4);
    assert!(history["messages"].as_array().unwrap().iter().all(|message| {
        message.get("tool_calls").is_none_or(|calls| calls.as_array().unwrap().is_empty())
    }), "live display events must not invent rich history metadata");
    client.close();
    export_tool_wire("lifecycle", &transcript);
}

#[test]
fn typed_tool_failure_precedes_the_unchanged_prompt_error() {
    let (_, mut client, sid) = typed_client(TypedOutcome::Fail);
    send_typed_prompt(&mut client, &sid, 3);
    let frames: Vec<Value> = (0..4).map(|_| client.read()).collect();
    assert_eq!(frames[2]["params"]["update"]["status"], "failed");
    assert_eq!(frames[2]["params"]["update"]["toolCallId"], frames[0]["params"]["update"]["toolCallId"]);
    assert_eq!(frames[3], rpc_error(json!(3), JSONRPC_INTERNAL_ERROR, "MCP tool failed".into()));
    client.close();
    export_tool_wire("failure", &frames);
}

#[test]
fn typed_tool_cancellation_flushes_failed_update_before_cancelled_reply_or_suppresses_on_eof() {
    for eof in [false, true] {
        let (observed, observed_rx) = mpsc::channel();
        let (_, mut client, sid) = typed_client(TypedOutcome::Cancel(observed));
        send_typed_prompt(&mut client, &sid, 3);
        let mut frames = vec![client.read(), client.read()];
        if eof {
            client.input.shutdown(Shutdown::Write).unwrap();
        } else {
            client.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}}));
        }
        observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        if eof {
            let mut tail = String::new();
            client.output.read_to_string(&mut tail).unwrap();
            assert!(tail.is_empty(), "closed output emitted: {tail}");
            client.worker.take().unwrap().join().unwrap().unwrap();
        } else {
            frames.extend([client.read(), client.read()]);
            assert_eq!(frames[2]["params"]["update"]["status"], "failed");
            assert_eq!(frames[2]["params"]["update"]["content"][0]["content"]["text"], "Cancelled");
            assert_eq!(frames[3], rpc_ok(json!(3), json!({"stopReason":"cancelled"})));
            // Cancel is one-way; the next response must belong to this request.
            assert_eq!(client.request(4, "initialize", json!({"protocolVersion":1}))["id"], 4);
            client.close();
            export_tool_wire("cancel", &frames);
        }
    }
}

struct ToolOutputFailure {
    stream: TcpStream,
    attempts: Arc<AtomicU64>,
}
impl Write for ToolOutputFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if String::from_utf8_lossy(bytes).contains("\"sessionUpdate\":\"tool_call\"") {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "tool output closed"));
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn typed_tool_output_failure_cancels_executor_without_waiting_for_input() {
    let (observed, observed_rx) = mpsc::channel();
    let engine = Arc::new(AcpEngine::new(AcpAdmissionPolicy::default(), Arc::new(TypedExecutor(TypedOutcome::Cancel(observed)))));
    let attempts = Arc::new(AtomicU64::new(0));
    let writes = Arc::clone(&attempts);
    let mut client = Client::start_with(engine, move |stream| ToolOutputFailure {stream, attempts: writes});
    client.initialize();
    let sid = client.session()["result"]["sessionId"].as_str().unwrap().to_owned();
    send_typed_prompt(&mut client, &sid, 3);
    observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    client.input.shutdown(Shutdown::Write).unwrap();
    assert_eq!(client.worker.take().unwrap().join().unwrap().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    let mut tail = String::new();
    client.output.read_to_string(&mut tail).unwrap();
    assert!(tail.is_empty());
}

struct LegacyOwnedExecutor;
impl AcpTurnExecutor for LegacyOwnedExecutor {
    fn execute_turn(&self, _: &AcpTurnContext, _: &AtomicBool, _: &mut dyn FnMut(&str)) -> Result<Vec<AcpContentBlock>, String> {
        panic!("legacy owned-token override must be preserved")
    }
    fn execute_turn_with_cancel_token(&self, _: &AcpTurnContext, _: Arc<AtomicBool>, emit: &mut dyn FnMut(&str)) -> Result<AcpExecutorOutput, String> {
        emit("legacy answer");
        Ok(AcpExecutorOutput {
            blocks: vec![AcpContentBlock::text("legacy answer")],
            tool_calls: vec![AcpToolCallMeta {id:"legacy-id".into(), name:"legacy-tool".into(), arguments:json!({"arg":1}), status:Some("legacy-status".into())}],
        })
    }
}

#[test]
fn typed_default_preserves_legacy_owned_override_history_and_non_send_callback() {
    let engine = AcpEngine::new(AcpAdmissionPolicy::default(), Arc::new(LegacyOwnedExecutor));
    let session = engine.dispatch_session_new(&json!({})).unwrap()["session_id"].as_str().unwrap().to_owned();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let callback_state = std::rc::Rc::clone(&observed);
    let result = engine.dispatch_prompt(&json!({"session_id":session,"prompt":"hello"}), &mut |event| callback_state.borrow_mut().push(event)).unwrap();
    let events = observed.borrow();
    assert_eq!(events.iter().map(|event| event.kind.as_str()).collect::<Vec<_>>(), ["turn_started", "message_delta", "tool_call", "turn_completed"]);
    assert_eq!(events.iter().map(|event| event.seq).collect::<Vec<_>>(), [1,2,3,4]);
    let expected = json!({"id":"legacy-id","name":"legacy-tool","arguments":{"arg":1},"status":"legacy-status"});
    assert_eq!(serde_json::from_str::<Value>(events[2].delta.as_ref().unwrap()).unwrap(), expected);
    assert_eq!(result["tool_calls"], json!([expected]));
    assert_eq!(result["messages"][1]["tool_calls"], result["tool_calls"]);
    assert_eq!(result["output"], json!([{"type":"text","text":"legacy answer"}]));
    assert_eq!(result["receipt_sha256"].as_str().unwrap().len(), 64);
}
