//! Staged ACP v1 stdio adapter. Legacy engine/HTTP serialization is unchanged.
//! Nonempty MCP configurations are refused until the required bridge is available.
use super::*;
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, ScopedJoinHandle};

struct Output<W> {
    writer: Mutex<W>,
    closed: AtomicBool,
}
impl<W: Write> Output<W> {
    fn send(&self, value: &Value) -> io::Result<()> {
        self.send_after(value, || {})
    }
    fn send_after(&self, value: &Value, before_write: impl FnOnce()) -> io::Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        before_write();
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        let result = writer.write_all(&line).and_then(|()| writer.flush());
        if result.is_err() {
            self.close();
        }
        result
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

type ActiveTurns = Mutex<BTreeMap<String, String>>;

// Bound acquisition itself, including input without a newline. On overflow the
// connection is rejected; its remaining tail is never interpreted as a request.
fn read_frame(reader: &mut impl BufRead, frame: &mut Vec<u8>) -> io::Result<bool> {
    frame.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(!frame.is_empty());
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let length = newline.map_or(available.len(), |n| n + 1);
        let length = length.min(MAX_STDIO_LINE_BYTES + 2 - frame.len());
        frame.extend_from_slice(&available[..length]);
        reader.consume(length);
        if frame.last() == Some(&b'\n') {
            return Ok(true);
        }
        if frame.len() == MAX_STDIO_LINE_BYTES + 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACP stdio frame exceeds maximum allowed size",
            ));
        }
    }
}

fn cancel(engine: &AcpEngine, session: &str, turn: &str) {
    let _ = engine.dispatch_cancel(&json!({"session_id": session, "turn_id": turn}));
}
fn cancel_all(engine: &AcpEngine, active: &ActiveTurns) {
    for (session, turn) in active.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        cancel(engine, session, turn);
    }
}

fn join(worker: ScopedJoinHandle<'_, io::Result<()>>) -> io::Result<()> {
    worker
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("ACP prompt worker panicked")))
}

impl AcpStdioTransport {
    /// Serve legacy Susi ACP or staged official ACP v1 on one stdio connection.
    /// An integer `initialize.protocolVersion` selects v1; legacy callers keep
    /// their existing wire shapes. Only this new entrypoint requires `Send`.
    /// Prompt workers allow cancellation while reading continues. EOF closes
    /// output, signals active turns and joins them; an executor that ignores its
    /// cancellation token, or an already-blocked reader/writer, can delay shutdown.
    /// Closing input prevents new output frames; an in-flight write cannot be revoked.
    /// The return count includes notifications.
    /// Frames are bounded before parsing; an oversized frame rejects the connection,
    /// rather than interpreting the remainder of that frame as another request.
    pub fn serve_negotiated_reader_writer<R: BufRead, W: Write + Send>(
        engine: &AcpEngine,
        mut reader: R,
        writer: W,
    ) -> io::Result<usize> {
        let output = Output {
            writer: Mutex::new(writer),
            closed: AtomicBool::new(false),
        };
        let active = ActiveTurns::default();
        thread::scope(|scope| {
            let mut workers: Vec<ScopedJoinHandle<'_, io::Result<()>>> = Vec::new();
            let result = (|| {
                let mut standard = false;
                let mut initialized = false;
                let mut count = 0usize;
                let mut frame = Vec::with_capacity(MAX_STDIO_LINE_BYTES + 2);
                loop {
                    let mut i = 0;
                    while i < workers.len() {
                        if workers[i].is_finished() {
                            join(workers.swap_remove(i))?;
                        } else {
                            i += 1;
                        }
                    }
                    if output.closed.load(Ordering::SeqCst) {
                        break;
                    }
                    match read_frame(&mut reader, &mut frame) {
                        Ok(false) => break,
                        Ok(true) => {}
                        Err(error) => {
                            if error.kind() == io::ErrorKind::InvalidData {
                                output.send(&rpc_error(
                                    Value::Null,
                                    JSONRPC_INVALID_REQUEST,
                                    error.to_string(),
                                ))?;
                            }
                            return Err(error);
                        }
                    }
                    if output.closed.load(Ordering::SeqCst) {
                        break;
                    }
                    let line = std::str::from_utf8(&frame)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                        .trim();
                    if line.len() > MAX_STDIO_LINE_BYTES {
                        output.send(&rpc_error(
                            Value::Null,
                            JSONRPC_INVALID_REQUEST,
                            "ACP stdio frame exceeds maximum allowed size".into(),
                        ))?;
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "ACP stdio frame exceeds maximum allowed size",
                        ));
                    }
                    if line.is_empty() {
                        continue;
                    }
                    count = count.saturating_add(1);
                    let parsed = serde_json::from_str::<Value>(line);
                    let numeric_init = parsed.as_ref().is_ok_and(|request| {
                        request["method"] == "initialize"
                            && request["params"]["protocolVersion"].is_number()
                    });
                    if !standard && !numeric_init {
                        if let Some(reply) = Self::handle_line(engine, line) {
                            let value = serde_json::from_str(&reply)?;
                            output.send(&value)?;
                        }
                        continue;
                    }
                    standard = true;
                    let request = match parsed {
                        Ok(request) => request,
                        Err(error) => {
                            output.send(&rpc_error(
                                Value::Null,
                                JSONRPC_PARSE_ERROR,
                                format!("Parse error: {error}"),
                            ))?;
                            continue;
                        }
                    };
                    // Recognized and unknown notifications stay one-way even when
                    // their parameters are malformed; never manufacture an id:null reply.
                    if request["jsonrpc"] == "2.0"
                        && request.get("id").is_none()
                        && request.get("method").is_some_and(Value::is_string)
                    {
                        if initialized && request["method"] == "session/cancel" {
                            if let Some(session) =
                                request["params"].get("sessionId").and_then(Value::as_str)
                            {
                                let turns = active.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(turn) = turns.get(session) {
                                    cancel(engine, session, turn);
                                }
                            }
                        }
                        continue;
                    }
                    let (id, method, params) = match validate_rpc_envelope(&request) {
                        Ok(parts) => parts,
                        Err(error) => {
                            output.send(&error)?;
                            continue;
                        }
                    };
                    let outcome = match method.as_str() {
                        "initialize" => initialize(engine, &params).inspect(|_| {
                            initialized = true;
                        }),
                        "session/new" if initialized => new_session(engine, &params),
                        "session/prompt" if initialized => match prompt(engine, &params) {
                            Ok(params) => {
                                let session =
                                    params["session_id"].as_str().unwrap_or_default().to_owned();
                                let turn =
                                    params["turn_id"].as_str().unwrap_or_default().to_owned();
                                let mut turns = active.lock().unwrap_or_else(|e| e.into_inner());
                                // Pair admission with cancel_all's lock: a fatal output
                                // closure either sees this turn or prevents its admission.
                                if output.closed.load(Ordering::SeqCst) {
                                    break;
                                }
                                if turns.contains_key(&session)
                                    || turns.len() >= engine.admission.max_sessions
                                {
                                    Err((ACP_ERR_CAPABILITY_REFUSED, "ACP session already has an active turn or the connection is at capacity".into()))
                                } else {
                                    turns.insert(session.clone(), turn.clone());
                                    drop(turns);
                                    let output = &output;
                                    let active = &active;
                                    let (started, ready) = mpsc::sync_channel(0);
                                    workers.push(scope.spawn(move || {
                                        let result = run_prompt(
                                            engine,
                                            output,
                                            active,
                                            (id, &params),
                                            started,
                                        );
                                        let finish = || {
                                            let mut turns =
                                                active.lock().unwrap_or_else(|e| e.into_inner());
                                            engine
                                                .state
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .turn_cancellations
                                                .remove(&(session.clone(), turn));
                                            turns.remove(&session);
                                        };
                                        // Acquire output order first, then release admission before
                                        // starting the terminal frame. No active lock spans I/O.
                                        let result = match result {
                                            Ok(reply) => output.send_after(&reply, finish),
                                            Err(error) => {
                                                finish();
                                                Err(error)
                                            }
                                        };
                                        if result.is_err() {
                                            cancel_all(engine, active);
                                        }
                                        result
                                    }));
                                    // Finish engine admission before accepting another session
                                    // creation, which could otherwise evict this queued session.
                                    let _ = ready.recv();
                                    continue;
                                }
                            }
                            Err(error) => Err(error),
                        },
                        _ => Err((
                            JSONRPC_METHOD_NOT_FOUND,
                            format!("Method '{method}' is not supported by this ACP v1 adapter"),
                        )),
                    };
                    output.send(&match outcome {
                        Ok(value) => rpc_ok(id, value),
                        Err((code, message)) => rpc_error(id, code, message),
                    })?;
                }
                Ok(count)
            })();
            // Closing never waits for backpressured output before signalling.
            output.close();
            cancel_all(engine, &active);
            let mut worker_error = None;
            for worker in workers {
                if let Err(error) = join(worker) {
                    worker_error.get_or_insert(error);
                }
            }
            result.and_then(|count| worker_error.map_or(Ok(count), Err))
        })
    }
}

fn invalid(message: &str) -> RpcErr {
    (JSONRPC_INVALID_PARAMS, message.to_owned())
}

fn initialize(engine: &AcpEngine, params: &Value) -> Result<Value, RpcErr> {
    if !params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .is_some_and(|v| v <= u16::MAX.into())
    {
        return Err(invalid(
            "protocolVersion must be an unsigned 16-bit integer",
        ));
    }
    Ok(json!({
        "protocolVersion": 1,
        "agentInfo": {"name": "susi", "version": engine.engine_version},
        "agentCapabilities": {"loadSession": false},
        "authMethods": []
    }))
}

fn new_session(engine: &AcpEngine, params: &Value) -> Result<Value, RpcErr> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("cwd must be an absolute path"))?;
    if !Path::new(cwd).is_absolute() {
        return Err(invalid("cwd must be an absolute path"));
    }
    AcpAdmissionPolicy::validate_workspace_path(cwd)?;
    let servers = params
        .get("mcpServers")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("mcpServers must be an array"))?;
    if !servers.is_empty() {
        return Err((
            ACP_ERR_CAPABILITY_REFUSED,
            "This staged ACP v1 adapter does not yet support supplied MCP servers".into(),
        ));
    }
    let session = engine.create_session(&json!({}), Some(cwd.to_owned()))?;
    Ok(json!({"sessionId": session["session_id"]}))
}

fn prompt(engine: &AcpEngine, params: &Value) -> Result<Value, RpcErr> {
    let session = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("sessionId must be a non-empty string"))?;
    if !engine
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .sessions
        .contains_key(session)
    {
        return Err((
            -32002, // Official ACP Resource not found; legacy code stays unchanged.
            format!("ACP session not found: {session}"),
        ));
    }
    let content = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("prompt must be a content block array"))?;
    let mut blocks = Vec::with_capacity(content.len());
    for block in content {
        let text = match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("text content requires a string text field"))?
                .to_owned(),
            Some("resource_link") => {
                if !block.get("name").is_some_and(Value::is_string)
                    || !block.get("uri").is_some_and(Value::is_string)
                {
                    return Err(invalid("resource_link requires string name and uri fields"));
                }
                // Preserve the reference and optional metadata for the text executor.
                // No URI is fetched, read, or granted additional access here.
                format!("Resource reference (not fetched): {block}")
            }
            _ => {
                return Err(invalid(
                    "Unsupported content type; this adapter accepts text and resource_link",
                ))
            }
        };
        blocks.push(AcpContentBlock::text(text));
    }
    let turn = engine.next_id.fetch_add(1, Ordering::Relaxed);
    Ok(json!({"session_id": session, "turn_id": format!("acp-v1-turn-{turn}"), "prompt": blocks}))
}

fn run_prompt<W: Write>(
    engine: &AcpEngine,
    output: &Output<W>,
    active: &ActiveTurns,
    (id, params): (Value, &Value),
    started: mpsc::SyncSender<()>,
) -> io::Result<Value> {
    let session = params["session_id"].as_str().unwrap_or_default();
    let turn = params["turn_id"].as_str().unwrap_or_default();
    let message_id = format!("{turn}-message");
    let mut streamed = String::new();
    let mut write_error = None;
    let mut started = Some(started);
    let result = engine.dispatch_prompt(params, &mut |event| {
        if event.kind == "turn_started" {
            if let Some(started) = started.take() {
                let _ = started.send(());
            }
        }
        if event.kind == "message_delta" {
            if let Some(text) = event.delta {
                if text.is_empty() {
                    return;
                }
                streamed.push_str(&text);
                if let Err(error) = output.send(&message_chunk(session, text, &message_id)) {
                    write_error.get_or_insert(error);
                    cancel_all(engine, active);
                }
            }
        }
    });
    let reply = match result {
        Ok(result) => {
            if result["stop_reason"] != "cancelled" {
                if let Some(blocks) = result["output"].as_array() {
                    let canonical = blocks
                        .iter()
                        .filter_map(|block| block.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Some(suffix) = canonical.strip_prefix(&streamed) {
                        if !suffix.is_empty() {
                            output.send(&message_chunk(session, suffix.to_owned(), &message_id))?;
                        }
                    } else if !canonical.is_empty() {
                        // A different canonical result is a separate message, not
                        // discarded or appended as if it extended the partial text.
                        output.send(&message_chunk(
                            session,
                            canonical,
                            &format!("{turn}-result"),
                        ))?;
                    }
                }
            }
            let stop = match result["stop_reason"].as_str() {
                Some("cancelled") => "cancelled",
                Some("refused") => "refusal",
                _ => "end_turn",
            };
            rpc_ok(id, json!({"stopReason": stop}))
        }
        Err((code, message)) => rpc_error(
            id,
            if code == ACP_ERR_NOT_FOUND {
                -32002
            } else {
                code
            },
            message,
        ),
    };
    write_error.map_or(Ok(reply), Err)
}

fn message_chunk(session: &str, text: String, message_id: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "session/update", "params": {
        "sessionId": session, "update": {"sessionUpdate": "agent_message_chunk", "messageId": message_id, "content": {"type": "text", "text": text}}
    }})
}

#[cfg(test)]
#[path = "tests/stdio_v1.rs"]
mod tests;
