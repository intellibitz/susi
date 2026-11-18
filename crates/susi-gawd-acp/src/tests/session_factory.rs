// Included by stdio_v1.rs so these regressions exercise the same socket client.

type FactoryServerRecord = (String, String, Vec<String>, Vec<(String, String)>);

struct FactorySpecRecord {
    cwd: String,
    servers: Vec<FactoryServerRecord>,
    admission: AcpAdmissionPolicy,
}

struct FactorySetupEntered {
    label: String,
    cancelled: Arc<AtomicBool>,
}

struct FactoryShared {
    engine: Mutex<std::sync::Weak<AcpEngine>>,
    specs: Mutex<Vec<FactorySpecRecord>>,
    entered: mpsc::Sender<FactorySetupEntered>,
    executed: mpsc::Sender<String>,
    cancelled: mpsc::Sender<String>,
    closed: mpsc::Sender<(String, bool)>,
    release_setup: AtomicBool,
}

struct FactoryExecutor(Arc<FactoryShared>);
struct FactorySessionExecutor {
    shared: Arc<FactoryShared>,
    label: String,
}

impl AcpTurnExecutor for FactoryExecutor {
    fn execute_turn(
        &self,
        _: &AcpTurnContext,
        _: &AtomicBool,
        _: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        Err("supplied-server turn reached the shared factory executor".into())
    }

    fn create_session_executor(
        &self,
        spec: &AcpSessionSpec,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Arc<dyn AcpTurnExecutor>>, String> {
        self.0.specs.lock().unwrap().push(FactorySpecRecord {
            cwd: spec.cwd.clone(),
            servers: spec
                .mcp_servers
                .iter()
                .map(|server| {
                    (
                        server.name.clone(),
                        server.command.clone(),
                        server.args.clone(),
                        server
                            .env
                            .iter()
                            .map(|var| (var.name.clone(), var.value.clone()))
                            .collect(),
                    )
                })
                .collect(),
            admission: spec.admission.clone(),
        });
        let server = spec.mcp_servers.first().ok_or("fake requires a server")?;
        let label = server
            .env
            .iter()
            .find(|var| var.name == "LABEL")
            .map(|var| var.value.clone())
            .ok_or("fake requires LABEL")?;
        let _ = self.0.entered.send(FactorySetupEntered {
            label: label.clone(),
            cancelled,
        });
        if server.name == "blocked" {
            // Deliberately finish after cancellation: the transport owns rollback
            // even when a factory returns an executor after its token is set.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !self.0.release_setup.load(Ordering::SeqCst) {
                if std::time::Instant::now() >= deadline {
                    return Err("fake setup release timed out".into());
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(Some(Arc::new(FactorySessionExecutor {
            shared: Arc::clone(&self.0),
            label,
        })))
    }
}

impl AcpTurnExecutor for FactorySessionExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        let _ = self.shared.executed.send(self.label.clone());
        let text = format!("executor:{}", self.label);
        emit(&text);
        if ctx.joined_prompt_text() == "wait" {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !cancelled.load(Ordering::SeqCst) {
                if std::time::Instant::now() >= deadline {
                    return Err("fake turn cancellation timed out".into());
                }
                thread::sleep(Duration::from_millis(2));
            }
            let _ = self.shared.cancelled.send(self.label.clone());
        }
        Ok(vec![AcpContentBlock::text(text)])
    }

    fn close_session(&self) {
        let outside_engine_lock = self
            .shared
            .engine
            .lock()
            .unwrap()
            .upgrade()
            .is_none_or(|engine| engine.state.try_lock().is_ok());
        let _ = self
            .shared
            .closed
            .send((self.label.clone(), outside_engine_lock));
    }
}

struct FactoryHarness {
    engine: Arc<AcpEngine>,
    shared: Arc<FactoryShared>,
    entered: mpsc::Receiver<FactorySetupEntered>,
    executed: mpsc::Receiver<String>,
    cancelled: mpsc::Receiver<String>,
    closed: mpsc::Receiver<(String, bool)>,
}

impl FactoryHarness {
    fn new(admission: AcpAdmissionPolicy) -> Self {
        let (entered, entered_rx) = mpsc::channel();
        let (executed, executed_rx) = mpsc::channel();
        let (cancelled, cancelled_rx) = mpsc::channel();
        let (closed, closed_rx) = mpsc::channel();
        let shared = Arc::new(FactoryShared {
            engine: Mutex::new(std::sync::Weak::new()),
            specs: Mutex::new(Vec::new()),
            entered,
            executed,
            cancelled,
            closed,
            release_setup: AtomicBool::new(false),
        });
        let engine = Arc::new(AcpEngine::new(
            admission,
            Arc::new(FactoryExecutor(Arc::clone(&shared))),
        ));
        *shared.engine.lock().unwrap() = Arc::downgrade(&engine);
        Self {
            engine,
            shared,
            entered: entered_rx,
            executed: executed_rx,
            cancelled: cancelled_rx,
            closed: closed_rx,
        }
    }

    fn entered(&self, expected: &str) -> Arc<AtomicBool> {
        let entered = self.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(entered.label, expected);
        entered.cancelled
    }

    fn closed(&self, expected: &str) {
        let (label, outside_lock) = self.closed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(label, expected);
        assert!(
            outside_lock,
            "session cleanup must run outside engine mutex"
        );
    }
}

struct FactoryRelease(Arc<FactoryShared>);
impl Drop for FactoryRelease {
    fn drop(&mut self) {
        self.0.release_setup.store(true, Ordering::SeqCst);
    }
}

fn factory_wait(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

fn factory_params(name: &str, label: &str) -> Value {
    json!({
        "cwd": std::env::current_dir().unwrap().join("factory workspace "),
        "mcpServers": [{
            "name": name,
            // A valid absolute descriptor; the fake factory never launches it.
            "command": std::env::current_exe().unwrap(),
            "args": [" argument with spaces ", "", "--literal=$NO_EXPANSION"],
            "env": [{"name":"LABEL","value":label},{"name":"EMPTY","value":""}]
        }]
    })
}

fn factory_session(client: &mut Client, id: u64, label: &str) -> String {
    let reply = client.request(id, "session/new", factory_params("peer", label));
    reply["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("session creation failed: {reply}"))
        .to_owned()
}

fn factory_send_prompt(client: &mut Client, id: u64, session: &str, text: &str) {
    client.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
        "params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}}));
}

fn factory_prompt(client: &mut Client, id: u64, session: &str) -> String {
    factory_send_prompt(client, id, session, "run");
    let mut text = String::new();
    loop {
        let reply = client.read();
        if reply.get("id").is_some() {
            assert_eq!(reply["id"], id);
            assert_eq!(reply["result"]["stopReason"], "end_turn", "{reply}");
            return text;
        }
        assert_eq!(reply["params"]["sessionId"], session);
        text.push_str(
            reply["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap(),
        );
    }
}

#[test]
fn session_factory_preserves_exact_spec_admission_and_same_cwd_isolation() {
    let mac = Arc::new(MacPolicy::new(
        [37; 32],
        mac_policy::PrivacyMode::Balanced,
        false,
    ));
    let admission = AcpAdmissionPolicy {
        mac_policy: Some(Arc::clone(&mac)),
        subject: "factory-specific-subject".into(),
        max_sessions: 7,
        max_prompt_bytes: 4096,
        max_history_messages: 11,
        max_history_bytes: 8192,
    };
    let harness = FactoryHarness::new(admission.clone());
    let mut client = Client::start(Arc::clone(&harness.engine));
    client.initialize();
    let a = factory_session(&mut client, 10, "first env ");
    let b = factory_session(&mut client, 11, "second env");
    assert_ne!(a, b);
    harness.entered("first env ");
    harness.entered("second env");
    {
        let specs = harness.shared.specs.lock().unwrap();
        assert_eq!(specs.len(), 2);
        for (spec, label) in specs.iter().zip(["first env ", "second env"]) {
            let expected = factory_params("peer", label);
            assert_eq!(spec.cwd, expected["cwd"].as_str().unwrap());
            assert_eq!(spec.servers.len(), 1);
            let (name, command, args, env) = &spec.servers[0];
            assert_eq!(name, "peer");
            assert_eq!(
                command,
                expected["mcpServers"][0]["command"].as_str().unwrap()
            );
            assert_eq!(
                args,
                &[" argument with spaces ", "", "--literal=$NO_EXPANSION"]
            );
            assert_eq!(
                env,
                &vec![
                    ("LABEL".to_owned(), label.to_owned()),
                    ("EMPTY".to_owned(), String::new())
                ]
            );
            assert!(Arc::ptr_eq(
                spec.admission.mac_policy.as_ref().unwrap(),
                &mac
            ));
            assert_eq!(spec.admission.subject, admission.subject);
            assert_eq!(spec.admission.max_sessions, admission.max_sessions);
            assert_eq!(spec.admission.max_prompt_bytes, admission.max_prompt_bytes);
            assert_eq!(
                spec.admission.max_history_messages,
                admission.max_history_messages
            );
            assert_eq!(
                spec.admission.max_history_bytes,
                admission.max_history_bytes
            );
        }
    }
    assert_eq!(factory_prompt(&mut client, 20, &a), "executor:first env ");
    assert_eq!(factory_prompt(&mut client, 21, &b), "executor:second env");
    assert_eq!(factory_prompt(&mut client, 22, &a), "executor:first env ");
    client.close();
    let mut closed = Vec::new();
    for _ in 0..2 {
        let (label, outside_lock) = harness.closed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(outside_lock);
        closed.push(label);
    }
    closed.sort();
    assert_eq!(closed, ["first env ", "second env"]);
    assert!(harness.closed.try_recv().is_err());
}

#[test]
fn blocked_factory_setup_keeps_prompt_cancellation_live_and_capacity_global() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy {
        max_sessions: 2,
        ..Default::default()
    });
    let mut owner = Client::start(Arc::clone(&harness.engine));
    let mut other = Client::start(Arc::clone(&harness.engine));
    // Release setup before either Client joins on every assertion failure path.
    let release = FactoryRelease(Arc::clone(&harness.shared));
    owner.initialize();
    other.initialize();
    let sid = factory_session(&mut owner, 10, "active");
    harness.entered("active");
    factory_send_prompt(&mut owner, 11, &sid, "wait");
    assert_eq!(owner.read()["method"], "session/update");
    assert_eq!(
        harness
            .executed
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        "active"
    );
    owner.send(json!({"jsonrpc":"2.0","id":20,"method":"session/new",
        "params":factory_params("blocked","pending")}));
    let setup_cancel = harness.entered("pending");
    assert!(!setup_cancel.load(Ordering::SeqCst));
    assert_eq!(harness.engine.state.lock().unwrap().sessions.len(), 1);
    for (client, id) in [(&mut owner, 21), (&mut other, 30)] {
        let refused = client.request(id, "session/new", factory_params("blocked", "overflow"));
        assert_eq!(refused["id"], id);
        assert_eq!(
            refused["error"]["code"], ACP_ERR_CAPABILITY_REFUSED,
            "{refused}"
        );
    }
    assert_eq!(harness.shared.specs.lock().unwrap().len(), 2);
    assert!(harness.entered.try_recv().is_err());
    owner.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}}));
    assert_eq!(
        harness
            .cancelled
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        "active"
    );
    assert_eq!(
        owner.read(),
        json!({"jsonrpc":"2.0","id":11,"result":{"stopReason":"cancelled"}})
    );
    assert!(
        !setup_cancel.load(Ordering::SeqCst),
        "prompt cancellation must not cancel unrelated setup"
    );
    drop(release);
    let created = owner.read();
    assert_eq!(created["id"], 20);
    assert!(created["result"]["sessionId"].is_string(), "{created}");
    owner.close();
    other.close();
}

#[test]
fn pending_factory_reservation_refuses_new_turns_across_connections() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy {
        max_sessions: 1,
        ..Default::default()
    });
    let mut owner = Client::start(Arc::clone(&harness.engine));
    let mut creator = Client::start(Arc::clone(&harness.engine));
    let release = FactoryRelease(Arc::clone(&harness.shared));
    owner.initialize();
    creator.initialize();
    let idle = factory_session(&mut owner, 10, "idle");
    harness.entered("idle");
    creator.send(json!({"jsonrpc":"2.0","id":20,"method":"session/new",
        "params":factory_params("blocked","replacement")}));
    harness.entered("replacement");
    let refused = owner.request(
        30,
        "session/prompt",
        json!({"sessionId":idle,"prompt":[{"type":"text","text":"run"}]}),
    );
    assert_eq!(refused["id"], 30);
    assert_eq!(
        refused["error"]["code"], ACP_ERR_CAPABILITY_REFUSED,
        "{refused}"
    );
    assert!(harness.executed.try_recv().is_err());
    assert!(harness.engine.state.lock().unwrap().sessions[&idle]
        .summary
        .active_turn_id
        .is_none());
    drop(release);
    let created = creator.read();
    assert_eq!(created["id"], 20);
    let replacement = created["result"]["sessionId"].as_str().unwrap();
    harness.closed("idle");
    assert_eq!(
        factory_prompt(&mut creator, 40, replacement),
        "executor:replacement"
    );
    owner.close();
    creator.close();
    harness.closed("replacement");
    assert!(harness.closed.try_recv().is_err());
}

#[test]
fn disconnect_cancels_pending_setup_and_rolls_back_its_late_executor() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy {
        max_sessions: 1,
        ..Default::default()
    });
    let mut client = Client::start(Arc::clone(&harness.engine));
    let mut other = Client::start(Arc::clone(&harness.engine));
    let release = FactoryRelease(Arc::clone(&harness.shared));
    client.initialize();
    other.initialize();
    client.send(json!({"jsonrpc":"2.0","id":20,"method":"session/new",
        "params":factory_params("blocked","late")}));
    let cancelled = harness.entered("late");
    let refused = other.request(21, "session/new", factory_params("blocked", "overflow"));
    assert_eq!(
        refused["error"]["code"], ACP_ERR_CAPABILITY_REFUSED,
        "{refused}"
    );
    assert_eq!(harness.shared.specs.lock().unwrap().len(), 1);
    assert!(harness.entered.try_recv().is_err());
    client.input.shutdown(Shutdown::Write).unwrap();
    factory_wait(|| cancelled.load(Ordering::SeqCst));
    assert!(harness.engine.state.lock().unwrap().sessions.is_empty());
    drop(release);
    let mut tail = String::new();
    client.output.read_to_string(&mut tail).unwrap();
    assert!(tail.is_empty(), "session ID escaped after EOF: {tail}");
    client.worker.take().unwrap().join().unwrap().unwrap();
    harness.closed("late");
    assert!(harness.engine.state.lock().unwrap().sessions.is_empty());
    assert!(harness.executed.try_recv().is_err());
    // Rollback releases admission as well as the returned executor.
    let mut recovered = Client::start(Arc::clone(&harness.engine));
    recovered.initialize();
    factory_session(&mut recovered, 30, "recovered");
    recovered.close();
    other.close();
    harness.closed("recovered");
    assert!(harness.closed.try_recv().is_err());
}

#[test]
fn idle_eviction_and_legacy_close_cleanup_once_outside_engine_mutex() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy {
        max_sessions: 1,
        ..Default::default()
    });
    let mut client = Client::start(Arc::clone(&harness.engine));
    client.initialize();
    let a = factory_session(&mut client, 10, "evicted");
    let b = factory_session(&mut client, 11, "closed");
    harness.closed("evicted");
    {
        let state = harness.engine.state.lock().unwrap();
        assert!(!state.sessions.contains_key(&a));
        assert!(state.sessions.contains_key(&b));
    }
    let reply = harness.engine.handle_jsonrpc_value(
        json!({"jsonrpc":"2.0","id":12,"method":"session/close","params":{"session_id":b}}),
        &mut |_| {},
    );
    assert_eq!(reply["result"]["closed"], true, "{reply}");
    harness.closed("closed");
    client.close();
    assert!(
        harness.closed.try_recv().is_err(),
        "stale connection ownership closed a session twice"
    );
}

#[test]
fn connection_ownership_confines_prompt_cancel_and_eof_cleanup() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy::default());
    let legacy = harness.engine.dispatch_session_new(&json!({})).unwrap();
    let legacy_id = legacy["session_id"].as_str().unwrap().to_owned();
    let mut a = Client::start(Arc::clone(&harness.engine));
    let mut b = Client::start(Arc::clone(&harness.engine));
    a.initialize();
    b.initialize();
    let a_id = factory_session(&mut a, 10, "owner-a");
    let b_id = factory_session(&mut b, 10, "owner-b");
    for forbidden in [&a_id, &legacy_id] {
        let refused = b.request(
            20,
            "session/prompt",
            json!({"sessionId":forbidden,"prompt":[{"type":"text","text":"run"}]}),
        );
        assert_eq!(refused["error"]["code"], -32002, "{refused}");
    }
    assert!(harness.executed.try_recv().is_err());
    factory_send_prompt(&mut a, 30, &a_id, "wait");
    assert_eq!(a.read()["method"], "session/update");
    harness
        .executed
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    b.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":a_id}}));
    assert_eq!(b.request(31, "session/list", json!({}))["id"], 31);
    assert!(harness.cancelled.try_recv().is_err());
    assert!(
        !harness.engine.state.lock().unwrap().active_cancellations[&a_id].load(Ordering::SeqCst)
    );
    a.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":a_id}}));
    assert_eq!(a.read()["result"]["stopReason"], "cancelled");
    a.close();
    harness.closed("owner-a");
    {
        let state = harness.engine.state.lock().unwrap();
        assert!(!state.sessions.contains_key(&a_id));
        assert!(state.sessions.contains_key(&b_id));
        assert!(state.sessions.contains_key(&legacy_id));
    }
    assert_eq!(factory_prompt(&mut b, 40, &b_id), "executor:owner-b");
    b.close();
    harness.closed("owner-b");
    assert!(harness
        .engine
        .state
        .lock()
        .unwrap()
        .sessions
        .contains_key(&legacy_id));
    harness
        .engine
        .dispatch_session_close(&json!({"session_id":legacy_id}))
        .unwrap();
    assert!(harness.closed.try_recv().is_err());
}

#[test]
fn malformed_supplied_server_descriptors_are_rejected_before_factory_admission() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy::default());
    let mut client = Client::start(Arc::clone(&harness.engine));
    client.initialize();
    let valid = factory_params("peer", "valid");
    let server = valid["mcpServers"][0].clone();
    let mut invalid_servers = vec![Value::Null, json!({}), json!([null]), json!([{}])];
    invalid_servers.push(json!([server.clone(), server.clone()]));
    for key in ["name", "command", "args", "env"] {
        let mut missing = server.clone();
        missing.as_object_mut().unwrap().remove(key);
        invalid_servers.push(json!([missing]));
        let mut wrong_type = server.clone();
        wrong_type[key] = json!(42);
        invalid_servers.push(json!([wrong_type]));
    }
    for (key, value) in [
        ("command", json!("relative-command")),
        ("args", json!([1])),
        ("env", json!([null])),
        ("env", json!([{"name":"KEY"}])),
        ("env", json!([{"value":"value"}])),
        ("env", json!([{"name":1,"value":"value"}])),
        ("env", json!([{"name":"KEY","value":1}])),
        (
            "env",
            json!([{"name":"LABEL","value":"first"},{"name":"LABEL","value":"second"}]),
        ),
    ] {
        let mut malformed = server.clone();
        malformed[key] = value;
        invalid_servers.push(json!([malformed]));
    }
    for servers in invalid_servers {
        let params = json!({"cwd":valid["cwd"],"mcpServers":servers});
        let reply = client.request(20, "session/new", params.clone());
        assert!(
            reply.get("error").is_some(),
            "accepted malformed descriptor: {params}"
        );
        assert!(harness.shared.specs.lock().unwrap().is_empty());
        assert!(harness.engine.state.lock().unwrap().sessions.is_empty());
    }
    assert!(harness.entered.try_recv().is_err());
    factory_session(&mut client, 30, "valid");
    assert_eq!(harness.shared.specs.lock().unwrap().len(), 1);
    client.close();
    harness.closed("valid");
}

struct FactoryFailingWriter {
    stream: TcpStream,
    failed: mpsc::Sender<()>,
}
impl Write for FactoryFailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(bytes);
        if text.contains("\"id\":20") && text.contains("\"sessionId\"") {
            let _ = self.failed.send(());
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "factory response output failed",
            ));
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[test]
fn failed_new_session_response_rolls_back_executor_and_owned_sessions() {
    let harness = FactoryHarness::new(AcpAdmissionPolicy::default());
    let (failed, failed_rx) = mpsc::channel();
    let mut client = Client::start_with(Arc::clone(&harness.engine), move |stream| {
        FactoryFailingWriter { stream, failed }
    });
    client.initialize();
    factory_session(&mut client, 10, "published");
    client.send(json!({"jsonrpc":"2.0","id":20,"method":"session/new",
        "params":factory_params("peer","unpublished")}));
    failed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut labels = Vec::new();
    for _ in 0..2 {
        let (label, outside_lock) = harness.closed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(outside_lock);
        labels.push(label);
    }
    labels.sort();
    assert_eq!(labels, ["published", "unpublished"]);
    assert!(harness.engine.state.lock().unwrap().sessions.is_empty());
    client.input.shutdown(Shutdown::Write).unwrap();
    let error = client.worker.take().unwrap().join().unwrap().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(harness.closed.try_recv().is_err());
}
