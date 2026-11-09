//! Wire-level tests exercise the same rmcp service used by both entry points.
use crate::mcp::{model::*, ServiceExt};
use crate::protocol::GmcpService;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Serializes every test in this crate that replaces the process-global
/// `PlaneBus` handlers for `TOOLS_LIST` / `TOOLS_EXISTS` / `TOOLS_EXECUTE`.
/// `GmcpService::tools()` and `tools/call` read those topics, so two such tests
/// running in parallel each served the other's stub: `mission_run` became an
/// "Unknown tool" whenever the extension-pack router registered mid-test.
pub(crate) static TOOLS_PLANE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type Wire = BufReader<DuplexStream>;
fn service() -> GmcpService {
    GmcpService::new(std::path::PathBuf::from("."))
}
async fn wire(service: GmcpService) -> Wire {
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let running = service.serve(server).await.unwrap();
        running.waiting().await.unwrap();
    });
    BufReader::new(client)
}
async fn send(wire: &mut Wire, message: Value) {
    wire.get_mut()
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}
async fn recv(wire: &mut Wire) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), wire.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
fn modern(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":{}});
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}
async fn discover(wire: &mut Wire) {
    send(wire, modern(1, "server/discover", json!({}))).await;
    let response = recv(wire).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["resultType"], "complete");
}
fn echo(service: &GmcpService) {
    let tool: Tool = serde_json::from_value(json!({"name":"test_echo", "inputSchema":{"type":"object"}, "outputSchema":{"type":"object"}})).unwrap();
    service
        .register_tool(
            tool,
            Arc::new(|request, _| {
                Box::pin(async move {
                    Ok(CallToolResult::structured(Value::Object(
                        request.arguments.unwrap_or_default(),
                    ))
                    .into())
                })
            }),
        )
        .unwrap();
}
#[tokio::test]
async fn current_discovery_tools_structured_output_and_errors() {
    let service = service();
    echo(&service);
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    // The built-in registry exceeds one page, so walk every cursor (bounded).
    // Cursors are revision-bound and parallel tests mutate the process-wide
    // tool registry, so a "Catalog changed" answer restarts the listing
    // without a cursor, exactly as the error instructs a client to.
    let mut tools = Vec::new();
    let mut params = json!({});
    let mut complete = false;
    for id in 100..150 {
        send(&mut wire, modern(id, "tools/list", params)).await;
        let listed = recv(&mut wire).await;
        if listed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("Catalog changed"))
        {
            tools.clear();
            params = json!({});
            continue;
        }
        assert!(listed["result"].get("ttlMs").is_some(), "{listed}");
        tools.extend(listed["result"]["tools"].as_array().unwrap().clone());
        match listed["result"]["nextCursor"].as_str() {
            Some(cursor) => params = json!({"cursor": cursor}),
            None => {
                complete = true;
                break;
            }
        }
    }
    assert!(complete, "tools/list must finish paging within the bound");
    assert!(tools
        .iter()
        .any(|t| t["name"] == "test_echo" && t["inputSchema"]["type"] == "object"));
    send(
        &mut wire,
        modern(
            3,
            "tools/call",
            json!({"name":"test_echo","arguments":{"answer":42}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["structuredContent"]["answer"],
        42
    );
    send(
        &mut wire,
        modern(4, "tools/call", json!({"name":"no_such_tool"})),
    )
    .await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32602);
    let mut unsupported = modern(5, "tools/list", json!({}));
    unsupported["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    send(&mut wire, unsupported).await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32022);
}
#[tokio::test]
async fn legacy_initialization_and_ping() {
    let mut wire = wire(service()).await;
    send(&mut wire, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    send(
        &mut wire,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    send(&mut wire, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["id"], 2);
    assert!(response["result"].get("resultType").is_none());
}
#[tokio::test]
async fn resources_prompts_completion_and_subscription() {
    let service = service();
    service.register_resource(Resource::new("test://document", "document"), Arc::new(|_,_| Box::pin(async {
        let result: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":"test://document","mimeType":"text/plain","text":"hello"}],"ttlMs":0,"cacheScope":"private"})).unwrap();
        Ok(result.into())
    })));
    let prompt = serde_json::from_value(
        json!({"name":"greet","arguments":[{"name":"name","required":true}]}),
    )
    .unwrap();
    service.register_prompt(prompt, Arc::new(|request,_| Box::pin(async move {
        let result: GetPromptResult = serde_json::from_value(json!({"messages":[{"role":"user","content":{"type":"text","text":request.arguments.unwrap()["name"]}}]})).unwrap();
        Ok(result.into())
    })));
    service.register_completion(
        "greet".into(),
        Arc::new(|_, _| {
            Box::pin(async {
                Ok(serde_json::from_value(
                    json!({"completion":{"values":["world"],"total":1,"hasMore":false}}),
                )
                .unwrap())
            })
        }),
    );
    let mut wire = wire(service.clone()).await;
    discover(&mut wire).await;
    send(
        &mut wire,
        modern(2, "resources/read", json!({"uri":"test://document"})),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["contents"][0]["text"],
        "hello"
    );
    send(
        &mut wire,
        modern(
            3,
            "prompts/get",
            json!({"name":"greet","arguments":{"name":"world"}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["result"]["messages"][0]["content"]["text"],
        "world"
    );
    send(&mut wire, modern(4, "prompts/get", json!({"name":"greet"}))).await;
    assert_eq!(recv(&mut wire).await["error"]["code"], -32602);
    send(&mut wire, modern(5, "completion/complete", json!({"ref":{"type":"ref/prompt","name":"greet"},"argument":{"name":"name","value":"w"}}))).await;
    assert_eq!(
        recv(&mut wire).await["result"]["completion"]["values"][0],
        "world"
    );
    send(
        &mut wire,
        modern(
            6,
            "subscriptions/listen",
            json!({"notifications":{"resourceSubscriptions":["test://document"]}}),
        ),
    )
    .await;
    assert_eq!(
        recv(&mut wire).await["method"],
        "notifications/subscriptions/acknowledged"
    );
    service.resource_updated("test://document".into());
    let notification = recv(&mut wire).await;
    assert_eq!(notification["method"], "notifications/resources/updated");
    assert_eq!(notification["params"]["uri"], "test://document");
    send(
        &mut wire,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":6}}),
    )
    .await;
}
#[tokio::test]
async fn task_poll_update_and_cooperative_cancellation() {
    let service = service();
    let task = service.tasks.spawn(Default::default(), |context| {
        Box::pin(async move {
            context.cancelled().await;
            Err(crate::mcp::task_manager::TaskExit::Cancelled)
        })
    });
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let request = |id, method| {
        let mut request = modern(id, method, json!({"taskId":task.task_id}));
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
            json!({"extensions":{"io.modelcontextprotocol/tasks":{}}});
        request
    };
    send(&mut wire, request(2, "tasks/get")).await;
    let response = recv(&mut wire).await;
    assert_eq!(response["result"]["status"], "working", "{response}");
    send(&mut wire, request(3, "tasks/cancel")).await;
    assert!(recv(&mut wire).await.get("error").is_none());
    for id in 4..20 {
        send(&mut wire, request(id, "tasks/get")).await;
        let response = recv(&mut wire).await;
        if response["result"]["status"] == "cancelled" {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("task never observed cancellation");
}

#[tokio::test]
async fn http_discovery_header_validation_and_origin_protection() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use tower_service::Service;
    let mut http = crate::server::http_service(service());
    let request = |method: &str, host: &str, origin: Option<&str>| {
        let mut builder = hyper::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", host)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder
            .body(Full::new(Bytes::from(
                modern(1, "server/discover", json!({})).to_string(),
            )))
            .unwrap()
    };
    let response = http
        .call(request("server/discover", "localhost", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::OK);
    assert!(response.headers().get("Mcp-Session-Id").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("susi-gmcp"));
    let response = http
        .call(request("tools/list", "localhost", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
    let response = http
        .call(request("server/discover", "attacker.invalid", None))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
    let response = http
        .call(request(
            "server/discover",
            "localhost",
            Some("https://attacker.invalid"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn elicitation_mrtr_preserves_input_responses() {
    let service = service();
    let definition =
        serde_json::from_value(json!({"name":"ask","inputSchema":{"type":"object"}})).unwrap();
    service.register_tool(definition, Arc::new(|request,_| Box::pin(async move {
        if let Some(responses) = request.input_responses {
            return Ok(CallToolResult::structured(responses["answer"].clone()).into());
        }
        let input: InputRequiredResult = serde_json::from_value(json!({
            "resultType":"input_required","inputRequests":{"answer":{"method":"elicitation/create","params":{
                "mode":"form","message":"Choose a name", "requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}
            }}}
        })).unwrap();
        Ok(input.into())
    }))).unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let mut request = modern(2, "tools/call", json!({"name":"ask"}));
    request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
        json!({"elicitation":{"form":{}}});
    send(&mut wire, request.clone()).await;
    let response = recv(&mut wire).await;
    assert_eq!(
        response["result"]["resultType"], "input_required",
        "{response}"
    );
    request["id"] = json!(3);
    request["params"]["inputResponses"] =
        json!({"answer":{"action":"accept","content":{"name":"Ada"}}});
    send(&mut wire, request).await;
    assert_eq!(
        recv(&mut wire).await["result"]["structuredContent"]["content"]["name"],
        "Ada"
    );
}

#[tokio::test]
async fn schema_validation_prevents_execution_and_invalid_output() {
    let service = service();
    let tool = serde_json::from_value(json!({"name":"validate", "inputSchema":{"type":"object","$defs":{"positive":{"type":"integer","minimum":1}},"properties":{"n":{"$ref":"#/$defs/positive"}},"required":["n"]},"outputSchema":{"type":"object","required":["answer"]}})).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    service
        .register_tool(
            tool,
            Arc::new(move |_, _| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(CallToolResult::structured(json!({"wrong":42})).into()) })
            }),
        )
        .unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    send(
        &mut wire,
        modern(
            2,
            "tools/call",
            json!({"name":"validate","arguments":{"n":0}}),
        ),
    )
    .await;
    assert_eq!(recv(&mut wire).await["result"]["isError"], true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    send(
        &mut wire,
        modern(
            3,
            "tools/call",
            json!({"name":"validate","arguments":{"n":1}}),
        ),
    )
    .await;
    assert_eq!(recv(&mut wire).await["result"]["isError"], true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn task_tool_returns_handle_and_accepts_midflight_input() {
    let service = service();
    let tool = serde_json::from_value(json!({"name":"task_input","inputSchema":{"type":"object"}}))
        .unwrap();
    service.register_task_tool(tool, Arc::new(|_,context| Box::pin(async move {
        let request: InputRequest = serde_json::from_value(json!({"method":"elicitation/create","params":{"mode":"form","message":"Name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}})).unwrap();
        let input = context.request_input("name",request).await?;
        Ok(CallToolResult::structured(input))
    }))).unwrap();
    let mut wire = wire(service).await;
    discover(&mut wire).await;
    let request = |id, method, params| {
        let mut request = modern(id, method, params);
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] =
            json!({"extensions":{"io.modelcontextprotocol/tasks":{}},"elicitation":{"form":{}}});
        request
    };
    send(
        &mut wire,
        request(2, "tools/call", json!({"name":"task_input"})),
    )
    .await;
    let result = recv(&mut wire).await;
    assert_eq!(result["result"]["resultType"], "task", "{result}");
    let task_id = result["result"]["taskId"].clone();
    let mut ready = false;
    for id in 3..20 {
        send(
            &mut wire,
            request(id, "tasks/get", json!({"taskId":task_id})),
        )
        .await;
        let result = recv(&mut wire).await;
        if result["result"]["inputRequests"]["name"].is_object() {
            ready = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(ready);
    send(&mut wire, request(20,"tasks/update",json!({"taskId":task_id,"inputResponses":{"name":{"action":"accept","content":{"name":"Ada"}}}}))).await;
    assert!(recv(&mut wire).await.get("error").is_none());
    for id in 21..40 {
        send(
            &mut wire,
            request(id, "tasks/get", json!({"taskId":task_id})),
        )
        .await;
        let result = recv(&mut wire).await;
        if result["result"]["status"] == "completed" {
            assert!(result.to_string().contains("Ada"));
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("task never completed");
}

/// Every request method and response status the logged server answered.
type HttpLog = Arc<std::sync::Mutex<Vec<(hyper::Method, hyper::StatusCode)>>>;

/// The real Streamable HTTP service with `test_echo`, on a loopback port,
/// logging each request's method and response status.
async fn logged_http_service() -> (std::net::SocketAddr, HttpLog) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower_service::Service;
    let service = service();
    echo(&service);
    let http = crate::server::http_service(service);
    let seen: HttpLog = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (http, log) = (http.clone(), log.clone());
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req| {
                    let (mut http, log) = (http.clone(), log.clone());
                    async move {
                        let method = req.method().clone();
                        let response = http.call(req).await?;
                        log.lock().unwrap().push((method, response.status()));
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, seen)
}

/// The blocking peer client (`susi_core::mcp_client`) against the real
/// Streamable HTTP service: version negotiation from the `initialize`
/// result, the tool call itself, and session release via `DELETE`.
#[tokio::test(flavor = "multi_thread")]
async fn peer_client_round_trip_negotiates_and_releases_session() {
    let (addr, seen) = logged_http_service().await;
    let result = tokio::task::spawn_blocking(move || {
        crate::susi_core::mcp_client::call_tool(
            &addr.to_string(),
            "test_echo",
            &json!({"ping": "pong"}),
            None,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["structuredContent"], json!({"ping": "pong"}));
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|(m, s)| m == hyper::Method::DELETE && s.is_success()),
        "session not released: {seen:?}"
    );
}

/// The std-only wire client (`susi_wire_client::mcp`, used by the thin
/// CLI) against the same real service: it negotiates the same revision,
/// gets the same `tools/call` result as the peer client, and its `DELETE`
/// releases the session.
#[tokio::test(flavor = "multi_thread")]
async fn wire_client_matches_peer_client_and_releases_session() {
    let (addr, seen) = logged_http_service().await;
    let (wire, peer, version) = tokio::task::spawn_blocking(move || {
        let addr = addr.to_string();
        let args = json!({"ping": "pong"});
        let mut session =
            susi_wire_client::mcp::Session::open(&addr, None, Duration::from_secs(5)).unwrap();
        let version = session.protocol_version().to_string();
        let wire = session
            .call_tool("test_echo", &args, None, &mut |_| {})
            .unwrap();
        session.close().unwrap();
        let peer =
            crate::susi_core::mcp_client::call_tool(&addr, "test_echo", &args, None).unwrap();
        (wire, peer, version)
    })
    .await
    .unwrap();
    assert_eq!(version, susi_wire_client::mcp::PROTOCOL_VERSION);
    assert_eq!(wire["structuredContent"], json!({"ping": "pong"}));
    assert_eq!(wire, peer);
    let seen = seen.lock().unwrap();
    let deletes = seen
        .iter()
        .filter(|(m, s)| m == hyper::Method::DELETE && s.is_success())
        .count();
    assert_eq!(deletes, 2, "both sessions must be released: {seen:?}");
}

/// Wire-level test for D-SP-4 Stage 2d: sending `notifications/cancelled`
/// while `mission_run` is in a phase propagates cooperative cancellation
/// across the plane bus, stops `mission_run` at the next phase boundary, and
/// records status `"CANCELLED"` (`exit_code = 1`) in the mission trace;
/// synchronous tools other than `mission_run` keep their existing behaviour.
#[tokio::test(flavor = "multi_thread")]
async fn mission_run_cancellation_stops_at_phase_boundary_and_records_cancelled_trace() {
    use crate::susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};

    struct ToolListStub;
    impl PlaneHandler for ToolListStub {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!([
                { "name": "mission_run", "description": "Run a mission" },
                { "name": "sovereign_status", "description": "Check status" }
            ]))
        }
    }

    struct ToolExecStub {
        mission_done: tokio::sync::Notify,
        sync_cancel_gate: Arc<std::sync::atomic::AtomicBool>,
        sync_saw_cancel: Arc<std::sync::atomic::AtomicBool>,
        sync_done: tokio::sync::Notify,
    }
    impl PlaneHandler for ToolExecStub {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            let ws = payload
                .get("workspace")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let outcome = with_watched(&payload, || match name {
                "mission_run" => {
                    crate::susi_core::progress::report("Mission started");
                    let mut status = "SUCCESS";
                    for _ in 0..400 {
                        if crate::susi_core::progress::is_cancelled() {
                            status = "CANCELLED";
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Must not advance to the next phase when cancelled.
                    if status != "CANCELLED" {
                        crate::susi_core::progress::report("Running the agent swarm");
                    }
                    let exit_code = i32::from(status != "SUCCESS");
                    let trace_dir = ws.join(".susi");
                    let _ = std::fs::create_dir_all(&trace_dir);
                    let trace = json!({
                        "status": status,
                        "exit_code": exit_code,
                    });
                    let _ = std::fs::write(
                        trace_dir.join("last_mission_trace.json"),
                        trace.to_string(),
                    );
                    Ok(json!({ "text": trace.to_string() }))
                }
                "sovereign_status" => {
                    crate::susi_core::progress::report("Status started");
                    for _ in 0..200 {
                        if self
                            .sync_cancel_gate
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    std::thread::sleep(Duration::from_millis(60));
                    self.sync_saw_cancel.store(
                        crate::susi_core::progress::is_cancelled(),
                        std::sync::atomic::Ordering::Release,
                    );
                    Ok(json!({ "text": "ok" }))
                }
                other => Err(format!("unexpected tool {other}")),
            });
            if name == "mission_run" {
                self.mission_done.notify_waiters();
            } else if name == "sovereign_status" {
                self.sync_done.notify_waiters();
            }
            outcome
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let exec_stub = Arc::new(ToolExecStub {
        mission_done: tokio::sync::Notify::new(),
        sync_cancel_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        sync_saw_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        sync_done: tokio::sync::Notify::new(),
    });
    // Held to the end of the test: the service reads these global topics.
    let _tools_plane = TOOLS_PLANE_TEST_LOCK.lock().await;
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(ToolListStub));
    PlaneBus::global().register(topics::TOOLS_EXECUTE, exec_stub.clone());

    let service = GmcpService::new(dir.path().to_path_buf());
    let mut wire = wire(service).await;
    discover(&mut wire).await;

    let mission_notified = exec_stub.mission_done.notified();
    let mut call = modern(
        2,
        "tools/call",
        json!({
            "name": "mission_run",
            "arguments": { "intent": "audit repo" }
        }),
    );
    call["params"]["_meta"]["progressToken"] = json!("mission-1");
    send(&mut wire, call).await;

    // Wait for "Tool started" (0.0) and "Mission started" progress notifications.
    let started = recv(&mut wire).await;
    assert_eq!(started["method"], "notifications/progress", "{started}");
    assert_eq!(started["params"]["message"], "Tool started");
    let phase = recv(&mut wire).await;
    assert_eq!(phase["method"], "notifications/progress", "{phase}");
    assert_eq!(phase["params"]["message"], "Mission started");

    // Cancel the in-flight mission_run call while it is in the phase.
    send(
        &mut wire,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 2, "reason": "user hit Ctrl-C" }
        }),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), mission_notified)
        .await
        .expect("mission_run must stop at phase boundary after cancellation");

    let trace_text =
        std::fs::read_to_string(dir.path().join(".susi/last_mission_trace.json")).unwrap();
    let trace: Value = serde_json::from_str(&trace_text).unwrap();
    assert_eq!(trace["status"], "CANCELLED");
    assert_eq!(trace["exit_code"], 1);

    // Synchronous tools other than `mission_run` keep their existing behaviour.
    let sync_notified = exec_stub.sync_done.notified();
    let mut sync_call = modern(
        3,
        "tools/call",
        json!({
            "name": "sovereign_status",
            "arguments": {}
        }),
    );
    sync_call["params"]["_meta"]["progressToken"] = json!("status-1");
    send(&mut wire, sync_call).await;
    let _ = recv(&mut wire).await; // "Tool started"
    let sync_phase = recv(&mut wire).await;
    assert_eq!(sync_phase["params"]["message"], "Status started");
    send(
        &mut wire,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 3, "reason": "late cancel" }
        }),
    )
    .await;
    exec_stub
        .sync_cancel_gate
        .store(true, std::sync::atomic::Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), sync_notified)
        .await
        .expect("synchronous tool must run to completion");
    assert!(
        !exec_stub
            .sync_saw_cancel
            .load(std::sync::atomic::Ordering::Acquire),
        "synchronous registry tools other than mission_run must not receive a cancel flag"
    );
}
